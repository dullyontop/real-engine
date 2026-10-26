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
- **Edit operations.** Underneath the registry, every scene change is one of a closed set of operations (`re_command::Op`): `spawn`, `despawn`, `set_field`, `add_component`, `remove_component`, `reparent`, `rename`, `set_override`, `set_doc` (an edit of an open document at a JSON pointer, ADR-0044), plus `restore` (the inverse of `despawn`). Operations are plain JSON (`{"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mass", "value": 12.5}`), address entities by stable id (ADR-0005) and components and fields by name (ADR-0032), are validated before anything changes, and each returns its exact inverse.
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
- JSON has no comments, so each entity may carry a `note` string, its comment (entity-core track, M3). The editor rewrites scene files on every edit, so a JSONC comment would be lost at the first save; a `note` is kept by every save, by forks, snapshots and undo, and is set with the `set_note` operation. It is never simulation state: the game does not read it and it is not part of `state_hash`. Generated entities are not saved and refuse a note.
- At 100,100 entities the spike saved a scene to JSON text in about 0.9 s (40.5 MB; 21.3 MB with defaults omitted) and loaded it in about 1 s, dominated by the generic document tree. A typed fast path is a later optimisation.
- **Amendment (entity core, measured on `main` at ceedab3, 2026-10-24).** A typed bulk-spawn fast path was measured with `core_bench` (release) and deferred. For 100,100 entities: direct typed spawn about 121 ms, loading a `SceneDoc` about 335 ms, and 100,100 spawn operations in one transaction about 716 ms, all under the spike's figures above. No runtime path needs it yet: nothing spawns 100,000 documents at once, and procedural apply and streaming go through fragments. It is added when a consumer needs it.

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

**Status:** Accepted. Amended in M2: `verify-plaza` is required (M2_PLAN WP16). Amended in M3: `build-web` is required (WP29 PR "portable", #89), `build-windows` and `build-macos` are required (WP18 PR "blocking"), `verify-valley` is required (WP28, #130), `d2` is required, with goldens required on every platform (WP18 PR "d2", #100), and the three `build-package` jobs are required (WP19 PR "bundles", #161). The required checks are now 17; see the last amendment.

**Context.** Development containers are Linux with lavapipe, llvmpipe and Xvfb, and no audio device. Nothing has been verified on Windows or macOS.

**Decision.**
- The required jobs, all on Ubuntu, are exactly these (M1_PLAN refers to this list):
  - `fmt` (`cargo fmt --all --check`);
  - `clippy` (`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`);
  - `test` (`CARGO_BUILD_JOBS=2`, lavapipe). It has two steps: the non-GPU crates in parallel, then the GPU crates (`re_render`, `re_app`, `re_cli`, `re_editor`) with `--test-threads=1`. Since M2 a last step runs `cargo xtask members` over the compat members those tests reached;
  - `msrv` (toolchain 1.95.0, `cargo build --workspace --all-targets --locked`);
  - `deny` (`cargo deny check`, cargo-deny 0.20.2);
  - `xtask` (`cargo xtask check`: crate graph, duplicate versions, explain pages, schema drift);
  - `verify-demo` (`real verify --project examples/island --json`);
  - `xvfb-smoke` (`real run --frames 300 --exit-after` under Xvfb);
  - `verify-plaza` (added in M2: `real check --types` and `real verify --project examples/plaza --json` with a check of the report, then a windowed run under Xvfb).
- Windows (`windows-latest`) and macOS (`macos-latest`) run `cargo build --workspace --locked`, then the tests of the pure-Rust crates that need no GPU, no audio device and no C++ toolchain (`re_core`, `re_noise`, `re_types`, `re_reflect`, `re_scene`, `re_command`). Their status is reported on the PR, and they do not block a merge (`continue-on-error: true`).
- The workflow lives in `.github/workflows/ci.yml` and is written by M1 WP0 (M1_PLAN). Jobs for crates that do not exist yet skip themselves, so CI is green from the first PR.

**Alternatives.** All three operating systems blocking merges from day one. That is unverified, and it would stop every PR on platform issues we cannot reproduce.

**Consequences.** Making Windows and macOS block merges is an M3 exit item.

**Amendment (M3, WP18 PR "blocking"; accepted by the maintainer, 2026-10-20).** `build-windows` and `build-macos` are required, with no `continue-on-error`. Each runs the whole test suite: the non-GPU crates, then the GPU crates on DX12 (WARP) or Metal with `--test-threads=1`. Each then runs `real verify` on the island and the plaza.
- Golden images are blessed on Linux only until WP18 PR "d2" adds the dx12 and metal sets. Until then, a platform verify passes when every stage but goldens is ok and the goldens stage's only problems are missing images for that backend (`RE0510`), reported as a warning. A golden that exists and differs still fails, and so does a run with no adapter.
- A text file with CRLF line endings fails `fmt` (`.gitattributes` checks text out with LF everywhere, ADR-0036).
- `build-web` (WP29 PR "portable"): `cargo check --target wasm32-unknown-unknown` of the runtime crates that need no C++; WP29 PR "toolchain" extends it to a full link with Luau and Jolt.
- `main` has no branch protection, so "required" means the merge rule: a PR is squash-merged only when every required job is green.

**Amendment (M3, WP28, #130).** `verify-valley` is required: `real check --types` and `real verify --project examples/valley --json` with a check of the report (the walk, drive and cell-crossing replays each ran twice with the same `state_hash`, and at least six goldens match, on Vulkan and on GL), then a windowed run under Xvfb.

**Amendment (M3, WP18 PR "d2", #100; 2026-10-24).** `d2` is required. A `d2-run` job runs `cargo xtask d2` on Ubuntu, Windows and macOS for every replay of the island and the plaza, and `d2` compares the reports with `--require linux-x86_64,windows-x86_64,macos-aarch64` (ADR-0036). The dx12 and metal golden sets are committed, so the missing-set rule above is gone: `real verify` on Windows and macOS must pass every stage, goldens included, as on Linux. Golden sets now exist for vulkan, gl, dx12 and metal. The valley's replays join `cargo xtask d2` in a later PR.
- The merge rule is these 14 checks: `fmt`, `clippy`, `test`, `msrv`, `deny`, `xtask`, `verify-demo`, `xvfb-smoke`, `verify-plaza`, `verify-valley`, `build-windows`, `build-macos`, `build-web` and `d2`.
- `build-package` (ADR-0037) runs on every PR but is not required yet. It joins the list when WP19 PR "bundles" runs it on all three platforms.

**Amendment (M3, WP19 PR "bundles", #161).** `build-package` (Ubuntu 22.04), `build-package-windows` and `build-package-macos` are required: each builds the island, the plaza and the valley with `real build`, checks that the packaged games play as their projects do, and checks the platform's bundle (ADR-0037's amendment for that PR). The merge rule is now 17 checks: the 14 above and these three.

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

**Status:** Accepted. Amended by ADR-0037: `build` is a tooling subcommand, and a binary built with `re_cli`'s `game` feature runs only the game in its archive. As built (WP19 PR "archive", #121), the packaged runtime binary is `re_cli` built with `--no-default-features --features game`.

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

**Amendment (M2, WP13 #79; accepted by the maintainer, 2026-10-15).** A part outside the world limits keeps its frame without a physics body; it is reported once (`RE5006`, warning) and gets its body back when moved inside. It still renders and replicates. The ±5 km limit and the compat stud scale are unchanged. Scripts often park idle parts far away, so this is not a boot failure.

## ADR-0032: Component reflection: our own registry with a closed set of field types; no bevy_reflect

**Status:** Accepted. Evidence: the core spike (research bundle `spikes/core/`; published as `docs/research/core.md` in M1 WP0), including its bevy_reflect 0.20.0 comparison.

**Context.** Commands, the inspector, Luau, agents and scene files all read and write component fields by name. Scripts and agents must be able to define new components at run time, and those must be real ECS components: queried, change-tracked, saved and undone like native ones. Luau numbers are f64, while most engine fields are f32.

**Decision.**
- `re_reflect` holds a **component registry** (`Registry`) over bevy_ecs components. Each registered component has a `ComponentSchema`: name, doc, origin (native or script), and fields with a name, a `FieldType`, a doc and a default.
- **A closed set of field types**: `bool`, `i32`, `i64`, `f32`, `f64`, `vec3`, `quat`, `color`, `string`, `entity`, and unit enums. They map one-to-one onto Luau values, JSON and the typed properties the M2 layer needs. One dynamic `Value` type carries them.
- **Central coercion rules**: integer to float; integral float to integer; f64 to f32 with a range check; non-finite floats rejected; quaternions normalised; enums checked against their variants. A Luau `0.1` written to an f32 field reads back as f32 `0.1`.
- **Exact f32 text round trip.** Parsing text as f64 and casting to f32 is wrong for exactly 2 of the 4,278,190,080 finite f32 values (`7.038531e-26` and its negative, bits `0x15ae43fd` and `0x95ae43fd`). `f32_from_parsed_f64` re-parses the shortest decimal form when the cast is not exact; the spike's exhaustive check then found 0 mismatches over every finite f32. This fix comes with a required test.
- **Native components** are declared with the `component!` macro (fields, docs, defaults, and a `requires(...)` clause), which implements the `Reflect` trait. Since M3 (entity-core track), `#[derive(Reflect)]` from `re_reflect_derive` (syn 2, quote, proc-macro2; re-exported as `re_reflect::Reflect`) does the same for an ordinary struct: field docs from `///`, defaults from the type's own `Default`, `#[reflect(skip)]` for fields that are not data (caches, handles), and `#[reflect(crate = "...")]` when `re_reflect` is reached through another crate. The struct derives `Component`, `Clone` and `Default` itself; `requires` is bevy's own `#[require(...)]` attribute. `component!` stays for the built-ins.
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
- **A derive macro in M1** (`re_reflect_derive` with syn). Better errors and ordinary Rust structs, but not needed for M1's built-ins, so it waited for M3's entity-core track.

**Consequences.**
- The registry is about 400 lines, with our own error messages and coercion rules, and one model for native and script components.
- By-name access costs about 2× typed access with a pre-resolved key, and about 12× when every name is looked up; that is fine for scripts, commands and agents, while hot engine systems stay typed.
- **Amendment (M2, WP9 PR "containers").** The closed set grows by three container types. `list` holds items of one scalar or record type. `map` holds string keys and dynamically typed values: scalars, records, or lists of those; integers are stored as floats. `record` is a named, fixed tuple of scalar fields registered once per process (`RecordSchema::register`); registration refuses a changed shape, so the set stays closed. No container holds an entity reference. In files, map values that are not a bool, number or string carry a `$type` tag. `Value::diff` reports changed list indices, map keys or record fields, and an equal write trips no change detection. In Luau, a list is an array, a map a string-keyed table, and a record a frozen table until WP10 replaces it with userdata. Immutable components can be reflected; the registry writes them by replacement so their hooks see every change, which keeps `re_scene`'s `TagIndex` exact.
- **Amendment (M2, WP9 PR "skeleton").** A third component origin, `ComponentOrigin::Host`, joins `Native` and `Script`. The host registers such a component from its own data with `Registry::register_host`, with the same checks as a script component, before any scene loads. `World::save` leaves host components out of `script_components`, and a file stores their fields by name like a native component's, so a field missing from an older file takes its default. A scene that names a host component nobody registered fails to load with an unknown-component error. The compatibility layer's per-class `Compat_<Class>` components use this origin, so adding a property or an enum item never breaks a saved compat scene.
- **Note (entity core, 2026-10-24).** A typed bulk-spawn fast path was measured and deferred; the figures and the reason are in ADR-0016's amendment.

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

## ADR-0036: D2 determinism: Jolt's cross-platform build, one portable maths library, and a three-platform `state_hash` check

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP18). Built in WP18 PR "d2" (#100), with the departures in the amendment at the end. The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: the WP18 measurements (`research/m3/wp18/measurements.md`): the physics benchmarks built with and without oxijolt's `cross-platform-deterministic` feature, the platform C library against the `libm` crate, and Luau's maths builtins against Rust callbacks. WP22's vehicle measurements (`research/m3/wp22/measurements.md`) agree.

**Context.** ADR-0018 gave us D1: the same binary on the same machine replays to the same `state_hash`. It deferred D2, the same `state_hash` across machines and operating systems, to M3, with its seam "the oxijolt feature flag plus the harness itself". The M3 exit check is identical hashes for the island's, the plaza's and the valley's replays on Linux x86_64 and Windows x86_64 (M3_PLAN, WP18 and definition of done item 2); macOS is reported. Facts checked for this ADR:

- **oxijolt 1.1.0+jolt-5.6.0 has the feature** (`cross-platform-deterministic`, forwarding to `oxijolt-sys`, both MIT OR Apache-2.0). It sets CMake's `CROSS_PLATFORM_DETERMINISTIC` (`oxijolt-sys/build/cmake_options.rs`). Jolt then defines `JPH_CROSS_PLATFORM_DETERMINISTIC` publicly, never emits FMA (`Jolt.cmake`: `-mfma` and `JPH_USE_FMADD` only `if (USE_FMADD AND NOT CROSS_PLATFORM_DETERMINISTIC)`; `Core.h` repeats the guard for ARM), and joltc compiles with `/fp:precise` instead of `/fp:fast` on MSVC and `-ffp-contract=off` on Clang. It works in our source build: `default-features = false` keeps `prebuilt` off, and the feature only changes a CMake cache entry.
- **Today every x86_64 build of Jolt uses FMA.** joltc's `USE_AVX2` and `USE_FMADD` default to ON, so GCC and Clang compile Jolt with `-mavx2 ... -mfma` and MSVC with `/arch:AVX2`, which also enables FMA. FMA rounds once where `a * b + c` rounds twice, and the compilers fuse in different places. Without the feature, Linux and Windows builds cannot agree. AVX2 itself stays on with the feature, so the engine needs an AVX2 CPU on x86_64 either way.
- **Jolt's documentation** (`Docs/Architecture.md` at v5.6.0, "Deterministic Simulation"): with `CROSS_PLATFORM_DETERMINISTIC` "the library [is] approximately 8% slower but the simulation will be deterministic regardless of" compiler, configuration, OS and architecture, as long as the same source is compiled with the same defines. Every Jolt build checks a set of scenes across 14 configurations, including "Windows MSVC x86 64-bit with AVX2", "Linux gcc x86 64-bit with AVX2", "Linux clang x86 64-bit with AVX2" and "macOS clang ARM 64-bit with NEON". Its caveats for the application: listener callbacks (contact, activation, step) arrive in no fixed order; `GetActiveBodies` order is not deterministic; narrow-phase query results arrive in varying order; broad-phase queries are not deterministic; the FPU state (round to nearest, no denormal flush) must match; and the platform's `sin`, `cos`, `std::sort`, `std::push_heap` and `std::hash` must not be used in simulated code.
- **oxijolt's documentation** (`docs/determinism.md`): results are bit-identical for the same binary, initial state and call order with 1 or 4 worker threads, Rayon or inline job systems; each step's events are sorted into a canonical order before they are queued; characters, vehicles, ragdolls and constraints are numbered per world in creation order; `collide_shape` hits arrive in unspecified order and must be sorted by the caller; a `ContactListener` must decide only from its arguments and data fixed for the step. oxijolt's CI runs its determinism gates with the feature on Windows and Linux, but each gate compares runs on one machine, never across machines.
- **re_physics already meets the caller rules.** Commands are sorted before they apply (ADR-0008, `world.rs:371`); poses come out sorted by entity (`world.rs:1181`); ended contacts are sorted (`world.rs:603`); query results are sorted (`query.rs:222`); the parts contact listener reads only its arguments and per-step tables (`parts/listener.rs`). No code changes the floating-point control word, Rust and both C runtimes start in round-to-nearest without flush-to-zero, and oxijolt builds Jolt with `FLOATING_POINT_EXCEPTIONS_ENABLED=OFF`.
- **WP22's measurement:** with the feature on, a car's end pose on a 12 s path moved by 4.6 cm against the default build, and stayed bit-identical across reruns and thread counts. Turning the feature on changes results, so references must be recorded with it. The island's three replays agree the same way: each runs twice to one `state_hash` with the feature (D1 holds), and every hash differs from the default build's. No test in the repository pins a `state_hash` value (`real verify` compares two runs of the same build), so nothing needs re-recording when it is turned on.
- **Rust code.** rustc never fuses `a * b + c` into an FMA; `mul_add` is an explicit, correctly rounded FMA in hardware or software, so it is portable. `+ - * /`, `sqrt`, `abs`, `floor`, `ceil`, `round`, `trunc`, `%`, `min`, `max`, `to_radians` and conversions are exact or fully specified. The transcendental methods (`sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sin_cos`, the hyperbolic ones, `exp`, `exp2`, `exp_m1`, `ln`, `log`, `log2`, `log10`, `ln_1p`, `powf`, `cbrt`, `hypot`) call the platform's C library: glibc, the Windows UCRT or Apple's libm. `powi` is documented as having unspecified precision that may vary by platform. Measured: glibc and the portable `libm` crate disagree in at least one bit on 1% of `sinf` inputs, 0.9% of `expf`, 9% of `powf` and 2.4% of `sin` (f64). The same disagreement exists between any two C libraries.
- **Where those calls are** (`src/` of each crate, unit tests included): `re_compat` 46 (CFrame and Vector3 maths, tween easing, humanoid, constraints, and render, GUI and lighting code), `re_anim` 10 (tween easing), `re_render` 6, `re_editor` 5, `re_audio` 2, `re_geom` 2 (tests comparing its own `sin_cos` against std), and one each in `re_physics` (`powi` for a ball's inertia), `re_proc` (`powi` in scatter's spacing check), `re_script` (`powi` in a bound) and `re_app` (media). `re_noise` already has none by rule, and `re_geom::math` has its own portable `sin_cos`, built from `+ - *` only.
- **glam 0.34.1** routes its transcendental functions (`sin_cos`, `atan2`, `acos_approx`, used by `Quat::from_euler`, `from_axis_angle`, `from_rotation_x/y/z`, `slerp`, `to_euler` and others) through std, unless its `libm` feature is on, which sends them to the `libm` crate instead (`src/f32/math.rs`: `#[cfg(any(feature = "libm", ...))]`). Its SSE2 paths use no FMA unless the `fma` target feature is enabled, which our builds never do. Its NEON paths do use FMA (`vfmaq_f32` in `Quat` multiplication and `slerp`, `Vec4::lerp`, `Vec3A`), so arm64 results differ from x86_64 for the same calls.
- **The `libm` crate** 0.2.16 (MIT) is already in `Cargo.lock`, through naga and num-traits. It is the Rust port of musl's libm. Its only architecture-specific code uses `sqrt`, `fma` and `rint` instructions, which are exact, so it gives the same bits on every target.
- **Luau** (luau0-src 0.22.0+luau740 through mlua 0.12.2) implements `math.sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `exp`, `log`, `log10`, `pow` and `vector.angle` with the C library. Scripts reach most of them through FASTCALL builtins, which bypass the `math` table whenever the environment is safe (`lvmexecute.cpp`, `if (cl->env->safeenv)`), and the compiler constant-folds builtin calls with constant arguments on the compiling machine. The `^` operator calls the C library's `pow`, except for the constant exponents 2, 0.5 and 3, which the VM computes as `x*x`, `sqrt(x)` and `x*x*x` (`LOP_POWK`). mlua 0.12.2 has `Compiler::set_disabled_builtins` and `Lua::set_compiler`, which sets the compiler for every chunk a VM loads, `require` included. Every engine VM is created in `re_script::vm::create` or `create_compat`. Luau's arithmetic is IEEE double; on x86_64 the C++ is compiled without FMA (no `-mfma` or `/arch:AVX2` from the `cc` crate), so it is portable between Linux and Windows.
- **Measured cost** (the reference container, 4 vCPU Xeon 2.8 GHz): `pile_bench` with 4000 bodies on one thread +1.1% mean (median of six runs), and +4 to +7% with 4 threads, inside the ±10% run-to-run noise; `bigmap` at 250,000 parts no measurable cost (step p50 5.0 against 5.1 ms); WP22's vehicle fleet within noise. `libm` against glibc: `sinf` 20 against 6.5 ns, `powf` 65 against 9 ns, `atan2f` 8.6 against 13.9 ns, `expf` 7.7 against 4.4 ns. Luau through a Rust callback instead of a builtin: about 40–60 ns more per trigonometric call and 120 ns more per `pow`.

**Decision.**

*Physics: always the deterministic Jolt build.*
- The workspace line becomes `oxijolt = { version = "=1.1.0", default-features = false, features = ["cross-platform-deterministic"] }`. There is no switch: development runs, `real verify`, tests and packaged games (WP19) use the same build, so a replay recorded anywhere is checked against the same simulation. `docs/DEPENDENCIES.md`'s oxijolt row moves the feature from "Later" to its features.
- Stated cost: **the physics benchmarks may be at most 10% slower** in mean step time than the default build on the same machine (`pile_bench` 1000 and 4000 bodies, `bigmap` 250,000 parts). Measured today: within noise, at most +7%. WP18 PR "d2" reruns both in its description.

*Rust: one portable maths library, enforced by clippy.*
- `libm = "=0.2.16"` joins `[workspace.dependencies]`, and glam gets its `libm` feature. Every crate computes transcendental functions with `libm` (`libm::sinf`, `libm::atan2f`, `libm::powf` ...), or with `re_geom::math` where that crate is already a dependency.
- A root `clippy.toml` bans the platform methods with `disallowed-methods`, so `cargo clippy -D warnings` (a required job) rejects them anywhere in the workspace: `f32::` and `f64::` `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sin_cos`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`, `exp`, `exp2`, `exp_m1`, `ln`, `log`, `log2`, `log10`, `ln_1p`, `powf`, `powi`, `cbrt` and `hypot`, each with the reason "platform maths differ between C libraries; use libm (ADR-0036)". Checked on glam 0.34.1 and clippy 1.97: both the method form (`x.sin()`) and the path form (`f32::sin(x)`) are reported. A test that compares against std (as `re_geom`'s does) uses `#[expect(clippy::disallowed_methods, reason = "...")]`.
- The ban is workspace-wide, render, editor and audio code included. Those call sites are few (13), the cost is nanoseconds, and a value computed in drawing code cannot later leak into the simulation unnoticed.
- `powi` is replaced by multiplication (`r * r`), which is what every current call site means.

*Luau: deterministic `math`, no platform builtins.*
- `re_script::vm::create` and `create_compat` replace `math.sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `exp`, `log` (both arities), `log10`, `pow` and `vector.angle` with functions over `libm` (f64), before `sandbox(true)` freezes the globals. Behaviour is otherwise unchanged (argument checks, `log(x, base)`, NaN and infinity handling follow Luau's).
- `re_script::compile::compiler()` disables the same builtins (`set_disabled_builtins`), so no FASTCALL and no constant folding goes to the C library, and each VM calls `lua.set_compiler(compile::compiler())`, so every chunk is compiled the same way: scripts, modules through `require`, and the compat layer's own chunks, which today load with mlua's default compiler. WP19 compiles packaged bytecode with the same `compiler()` (ADR-0037), so packaged games get the same rules.
- **The `^` operator stays the platform's `pow`** for exponents other than 2, 0.5 and 3; Luau gives no hook for it short of patching the VM. This is documented in the scripting guide (`math.pow` is portable, `x ^ y` is not), and the `d2` job catches any replay that depends on it. The island's scripts use no `^`. If a project needs more, the static scan (ADR-0014) can warn on `^` with another exponent; that is left to `re_script`'s owner.

*The harness: `cargo xtask d2` (contract frozen at WP18 PR "d2").*
- `real run --headless --replay R --checkpoints K --json` (a small `re_cli` change through the maintainer) adds `"checkpoints": [{"frame": f, "state_hash": h}]` every `K` frames (default 60) and at the last frame, using `App::run_headless_observed` and the same headless configuration as `real verify`'s replay stage.
- `cargo xtask d2 --project P [--replay R]... [--checkpoints K] [--json]` builds `real` in release, runs every `tests/replays/*.replay.json` of the project (or the ones named), and prints one report: `{"schema": "real-engine/d2/1", "platform": {"os", "arch", "target"}, "project", "rustc", "replays": [{"file", "frames", "checkpoints": [...]}]}`. Its exit code is 0 when every run completed; it compares nothing by itself.
- `cargo xtask d2 compare REPORT... --require linux-x86_64,windows-x86_64` compares the reports' checkpoints replay by replay. It exits 1 when two required platforms differ, naming the replay and the first checkpoint that differs and the last that matched; platforms that are not required are reported as matching or not without failing. The first differing frame is what a developer bisects with D1's local tools (`real verify` names differing components within one machine).
- xtask stays a tooling crate: it runs the `real` binary and reads JSON, and links no engine crate.

*CI (ADR-0023 amendment).*
- A job `d2-run` on `ubuntu-latest`, `windows-latest` and `macos-latest` runs `cargo xtask d2` for `examples/island` and `examples/plaza` (and `examples/valley` once WP28 adds it), and uploads the reports. A job `d2` that needs all three, and runs even when the macOS run fails (`if: always()`, failing only when a required platform's report is missing), downloads them and runs `d2 compare --require linux-x86_64,windows-x86_64`. `d2` becomes a required check in WP18 PR "d2". macOS arm64 is reported in its summary, not required.
- The action versions for uploading and downloading artifacts are pinned to verified majors in the PR, as ADR-0023's other actions are.

**Alternatives.**
- **Fixed-point or a software float for the simulation.** Bit-exact by construction, but Jolt is a float library and the whole API is f32; out of the question for M3.
- **The feature behind an engine option** (on for `d2`, off otherwise). Two physics builds would give two sets of results, so a replay could pass `d2` and fail in the build players run. The measured cost does not justify that.
- **Patch Luau** (through `luau0-src`'s `LUAU_CXXFLAGS` or a fork) so its C maths calls a portable library. It would also fix `^`, but it means carrying a patched VM against every Luau update, and forcing a header into another crate's C++ build is fragile on MSVC. Rejected for M3; revisit if `^` turns out to matter.
- **Our own polynomial maths everywhere** instead of `libm`, as `re_geom::math` does. Fine for `sin_cos`, but `atan2`, `pow`, `exp` and `log` with correct edge cases are real work that `libm` already does, and it is already in the lockfile.
- **Ban platform maths only in "simulated" crates.** Cheaper by 13 call sites, but the boundary moves as code moves (the compat layer mixes simulation and drawing in one crate), and a per-crate `clippy.toml` would replace the root one rather than add to it.
- **glam's `scalar-math` feature**, so arm64 matches x86_64. It also changes the alignment of `Vec3A`, `Vec4` and `Mat4`, which the render crates upload with bytemuck, and it slows every vector operation on x86_64. Not needed for the M3 exit (Linux and Windows); see the macOS consequence below.

**Consequences.**
- One build of the engine for everyone, about the same speed as now. Replays recorded with an older build will give different hashes after PR "d2" (WP22 saw a 4.6 cm change on a vehicle path); since no repository test pins a hash, nothing needs re-recording, but WP22's reference poses and any later pinned values must be recorded with the feature on.
- **AVX2 is a hard requirement on x86_64** (it was already, through Jolt's defaults). WP19's packaged games state it, and a missing AVX2 is a crash at the first physics step rather than an error message unless WP19 checks the CPU at startup.
- Every crate gains `libm` as a direct dependency where it does maths; `docs/DEPENDENCIES.md` gets a `libm` row and glam's `libm` feature.
- Script authors get portable `math` functions at about 50 ns more per call. `^` with an unusual exponent is the one documented gap.
- **macOS arm64 is reported, not required, and is expected to differ** until two things change: glam's NEON paths use FMA (Quat products, `slerp`, `Vec4::lerp`), and Apple Clang contracts `a * b + c` into FMA by default in Luau's C++ (`-ffp-contract=on`; Jolt itself is covered by the feature). A follow-up can try `CXXFLAGS_aarch64_apple_darwin=-ffp-contract=off` in `.cargo/config.toml` and glam's `scalar-math` on arm64 only, and measure them; the `d2` job's macOS line shows whether it worked.
- Determinism still assumes the same engine source, the same `Cargo.lock` and default compiler flags. Building with `-C target-cpu=native` does not change Rust's results (rustc never fuses) but would let the `cc` crate's C++ use FMA if `CXXFLAGS` asked for it; the `d2` job builds with the defaults.
- Disabling the maths builtins changes `compile::compiler()`'s output, so ADR-0037's `COMPILER_ID` changes with PR "d2" (whichever of WP18's and WP19's PRs lands second sets the new string), and archives built before it are refused with `RE0602`.
- ADR-0043 already makes `^` a static-scan warning in Luau generator scripts; extending it to every script is `re_script`'s call, as above.
- GPU output stays out of scope (goldens keep their tolerance), as do other CPU architectures.

*Browser builds (wasm32) and online play (added after the owner's answer of 2026-10-15; WP29 and WP30 own the details).*
- **What carries over unchanged.** Wasm arithmetic is IEEE single and double with round-to-nearest and no flush-to-zero, and has no FMA outside the opt-in relaxed-SIMD proposal, so Rust code, Luau's VM arithmetic and the `libm` crate (pure Rust; its wasm32 module uses only exact `floor`, `ceil`, `trunc`, `sqrt` instructions) give the same bits as on x86_64. Jolt lists "WASM32 emscripten running in nodejs" and "WASM64" among its cross-platform-deterministic configurations. The harness report already names any `os`/`arch`, so a wasm run (in Node or a headless browser) is one more report for `d2 compare`.
- **NaN bits.** Wasm leaves the sign and payload of a NaN that an operation produces nondeterministic, and x86 (default NaN `0xFFC00000`) and ARM (`0x7FC00000`) already differ there. `state_hash` hashes floats by their bits (ADR-0018), so a NaN anywhere in the world would hash differently across platforms for no simulation reason. **Decision:** `re_scene`'s `state_hash` hashes every NaN as one canonical bit pattern (a one-line change in `hash.rs` through the maintainer, in PR "d2"); a NaN in simulated state is a bug to report anyway, not a value to compare.
- **glam is the open question for the web.** On wasm32 without `simd128` glam uses its scalar code, whose operation order differs from SSE2's: `Vec4::dot` is `((x + y) + z) + w` in scalar code and `(x + z) + (y + w)` in SSE2 (`src/sse2.rs`, `dot4_into_m128`), and the NEON paths fuse. So the web, like macOS arm64, matches x86_64 only if every platform uses the same glam path (its `scalar-math` feature everywhere, with the alignment change noted above) or simulated code avoids glam's SIMD types (`Vec3A`, `Vec4`, `Quat`, `Mat4`, `Affine3A`) in favour of `Vec3`/`Mat3` and explicit scalar maths, as `re_geom::math` does. This ADR does not choose: the web WP measures both on its build, and the `d2` job reports the web until then.
- **Luau and Jolt in the browser** need a C and C++ toolchain for wasm (emscripten or wasi-sdk) and a C library (emscripten's musl or wasi-libc); with this ADR's Luau rules the only maths that still reaches that library is `^`. Which wasm target, and how wgpu's WebGPU backend fits it, is the web WP's ADR.
- **Online play does not need lockstep.** Server-authoritative replication sends state, not inputs, so it works without D2; D2 makes client prediction, rollback and replay of recorded matches agree with the server. The network ADR decides which model it uses; if it chooses lockstep or rollback, `d2` becomes a correctness gate for multiplayer, and the web question above becomes required rather than reported.

**Amendment (WP18 PR "d2", #100; 2026-10-24).** What was built, where it differs from the Decision above:
- **glam's `scalar-math` is on for every target**, with its `libm` feature (ADR-0049 §11's recommendation). Mat4, Quat and Affine3A products then match on SSE2, NEON and wasm. This replaces the alternative rejected above.
- **`powi` is not banned.** It compiles to multiplications, or to compiler-builtins' `__powisf2`, the same code on every target. The root `clippy.toml` bans the other std transcendental methods, sorted by path, each with a reason that cites this ADR. Every former std call site calls `libm`.
- **FP contraction is off for C and C++ on aarch64.** `.cargo/config.toml` sets `CFLAGS` and `CXXFLAGS` to `-ffp-contract=off` for `aarch64-apple-darwin` and `aarch64-unknown-linux-gnu`, because Clang contracts `a * b + c` into an FMA by default there. This covers Luau's and Jolt's C++.
- **Luau maths** go through `re_script::portable_math`, installed in every VM before the sandbox freezes the tables: the `math.sin` family, `atan2`, `exp`, `log` (with base), `log10`, `pow` and `vector.angle`. The compiler disables the matching builtins. WP24's generator VM installs it too. `^` still uses the platform `pow`, except for exponents 2, 0.5 and 3.
- **`COMPILER_ID` is `"o1d1-m"`** (the portable-maths builtins disabled), the change the consequence above announced. Archives built before it are refused with `RE0602`.
- **`state_hash` format `state-hash/2`.** Each component hashes only the fields whose value differs from its registered default, by name, and every NaN hashes as one canonical NaN. Adding a defaulted field therefore leaves every hash unchanged. No file pins a hash, so the change was one-time and free.
- **macOS arm64 is required in `d2`**, not reported: its hashes matched Linux and Windows at every checkpoint from the first run with the deterministic build. The CI job compares with `--require linux-x86_64,windows-x86_64,macos-aarch64`.
- **The `std::time` clock ban** lives in WP29's wasm-only clippy pass (`tools/ci/clippy-web/clippy.toml`, ADR-0049 §5), not in the root `clippy.toml`. That file repeats the root file's libm list, and `cargo xtask clippy-web` checks that it does.
- The physics benchmarks stayed inside the 10% allowance (PR "d2" lists them).

**Amendment (#143, #145).** `App::state_hash` is the hash that run reports, `real run`, checkpoints and the `d2` job compare. It is the world's `state_hash` alone when nothing below applies, so earlier hashes and goldens are unchanged. When the physics world has ragdolls, it is a blake3 hash of the world's hash and the ragdolls' state bytes (in entity order: mode, strength and every part's position, rotation and velocities, with NaNs canonical). When any `Animator` has a bound graph, playing or paused, that result is hashed again with the graphs' state (ADR-0039's amendment).

## ADR-0037: Packaged games: one archive behind `ProjectFiles`, Luau bytecode, zstd, and per-platform bundles

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP19). The code waits for M2's definition of done (M3_PLAN, "How M3 starts") and WP18 PR "blocking". The departures from the plan listed at the end of the Decision are accepted. Evidence: the WP19 measurements (`research/m3/wp19/measurements.md`): the runtime-only `real` built and inspected on Linux x86_64, the glTF importer run on public sample models, Luau bytecode compiled and loaded with mlua 0.12.2, the archive index encoding timed, and four compressors compared on the same data.

**Context.** `real build` must turn a project into a standalone game: a runtime-only binary, one packed archive and the platform's bundle, with no tooling linked (M3 plan, WP19; ADR-0027). Facts checked for this ADR:
- **The runtime reads project files at about 40 call sites in seven crates**, each with `std::fs` on `project.join(rel)`: `re_app` (`real.toml`, the entry scene, graph files, compat default scripts), `re_assets` (sources, `.meta.json` sidecars, materials, a `.gltf`'s external buffers and images, the processed cache), `re_script` (`runtime.rs` script loads; `modules.rs`'s `ProjectRequirer`, which also asks `is_file` and `is_dir` and reads `.luaurc`), `re_compat` (`runtime/scripts.rs` reads script source as UTF-8 text), `re_audio` (WAV) and `re_proc` (graph file inputs and its node-output cache); `re_input` also reads replay files, which are run inputs rather than project content. A few sites do more than read a file: `ProjectRequirer` refuses symlinks that leave `scripts/` with `canonicalize` (`re_script/src/modules.rs:194`), the glTF importer joins external URIs to the file's directory (`gltf_import.rs:465`), and `material.rs:228` walks up directories to find `real.toml`. Runtime paths also **write** into the project: `.real/cache/assets` and `.real/cache/proc`, the crash hook, the compat default scripts under `.real/defaults/` (when missing or changed, `re_app/src/compat.rs:304`), new `.meta.json` sidecars with fresh uuids for assets that have none (`re_assets/src/meta.rs:100`) and sidecar renames (`server.rs:535`), `.real/out/` captures from runtime commands, `save_scene`, and the planned `.real/data/` (ADR-0048). An installed game can be read-only (a macOS `.app`, `Program Files`), so none of these may write next to the game.
- **Processed assets are much bigger than their sources** because `re_assets` caches decoded RGBA8 texels. Measured on public glTF sample models: DamagedHelmet 3.77 MB → 84.7 MB processed (22×), Avocado 8.1 → 50.4 MB, Fox 0.16 → 4.3 MB. Compressed with zstd level 9 the helmet is 19.0 MB, still 5.0× its source file (5.6× the source compressed the same way). Importing from source costs 786 ms for the helmet against 191 ms for a processed-cache hit; the Fox is 37 ms against 10 ms. WP20's texture encoder (ADR-0047) will change the texture numbers; GPU block formats are too slow to encode at load.
- **Compressors**, decoding the helmet's processed data on one core of a 2.8 GHz Xeon: zstd (the `zstd` crate 0.13.3 over libzstd 1.5.7, the same C library 0.14.2 builds) 400–700 MB/s at levels 3–9; deflate (miniz_oxide 0.8.9, already in the runtime through `png`) 310–340 MB/s and 12% bigger than zstd 9 at level 6; lz4_flex 0.11 740–900 MB/s but 1.8× bigger than zstd 9; ruzstd 0.8 (pure Rust zstd decoder) 140–150 MB/s. zstd 9 compresses at about 45 MB/s; level 19 saves another 12% at 1.5 MB/s. blake3 1.8.7 hashes at 3.7–5 GB/s, so checking every entry's hash on read adds about 10–17% to decoding it, and much less to a full asset load (the postcard decode after it costs more than the zstd step).
- **zstd crate facts.** `zstd` 0.14.2 (2026-10-10, BSD-3-Clause, MSRV 1.64) → `zstd-safe` 8.1.0 (BSD-3-Clause) → `zstd-sys` 2.1.1+zstd.1.5.7 (BSD-3-Clause). With `default-features = false` it builds the bundled libzstd with `cc` (no pkg-config, no download, no system library; `ldd` shows only libc and libgcc_s) and drops the legacy-format decoders and the dictionary builder. 0.13.3 (MIT) is the previous release. All three licenses are on the allow-list.
- **Luau bytecode** (mlua 0.12.2, Luau 0.740, bytecode version 9, type-info version 3). `mlua::chunk::Compiler` with the engine's options (optimisation 1, debug 1, from `re_script::compile::compiler()`) produces byte-identical output on repeated runs. mlua's default `Compiler`, which `re_compat::runtime::load_chunk` gets because it sets none, has the same options (optimisation 1, debug 1, type info 0, coverage 0; `mlua/src/chunk.rs:236`), so native and compat scripts compile to the same bytecode today. On the repository's 57 `.luau` files (56 KB, two of them deliberate syntax-error fixtures) bytecode is 67 KB, and loading every chunk takes 1.2 ms from bytecode against 7.8 ms from source. mlua detects bytecode with `luaL_isbytecode` (first byte below `\t`, or a version byte with a valid type-info byte) unless the chunk's `ChunkMode` is set. Luau's own sandbox guide: "the VM assumes that the bytecode was generated by the Luau compiler (which never produces invalid/unsafe bytecode)", and embedders should sign bytecode they receive. Debug level 1 keeps line numbers and function names, so tracebacks from a packaged game still name files and lines; it is not obfuscation (bytecode decompilers exist).
- **Index cost** (postcard, about 83 bytes per entry): 10,000 entries decode in 1.1 ms and 100,000 in 17 ms; a sorted-path binary search costs about 0.3 µs.
- **The runtime-only binary on Linux** (`cargo build --release -p re_cli --no-default-features`, 5 min 46 s from clean on 4 cores): 48.8 MB, 37.2 MB stripped. It links `libstdc++.so.6`, `libgcc_s.so.1`, `libasound.so.2` (cpal), `libudev.so.1` (gilrs), `libm` and `libc`, and loads its graphics and windowing libraries at run time (none of Vulkan, EGL, X11 or Wayland is `NEEDED`). Built on Ubuntu 24.04 it needs glibc 2.38 (`__isoc23_strtol` and friends from the C and C++ dependencies); `pidfd_spawnp` (2.39) is a weak reference. Ubuntu 22.04 ships glibc 2.35, so a build there should need at most that; not yet measured. The headless island runs 600 frames in 0.6 s.
- **Windows and macOS** (not yet run; WP18's runners verify them in PR "bundles"): MSVC links the C runtime dynamically unless `+crt-static` is set. The `cc` crate follows that flag for Luau and libzstd; `oxijolt-sys` builds Jolt with CMake and maps `crt-static` to the static runtime in its own `build.rs`. wgpu 30's default DX12 shader compiler (`Dx12Compiler::Auto`) uses a `dxcompiler.dll` found on the path, else FXC, which is part of Windows; so the compiler can differ by machine unless the engine names one. A Rust binary is a console program on Windows unless `#![windows_subsystem = "windows"]`. On Apple silicon every executable must be signed; Apple's linker signs ad hoc by default, and copying or renaming the file keeps that signature, but editing the binary breaks it.
- **Third-party notices.** `cargo tree -p re_cli --no-default-features -e normal --target <triple>` lists 289 third-party crates for Linux x86_64, 262 for Windows and 259 for macOS arm64. 272 of the Linux ones carry license files at the package root; the other 17 do not (egui's crates, kira, gilrs, oxijolt, mlua-sys and others). Some keep them deeper: `oxijolt-sys/vendor/JoltPhysics/LICENSE`, `epaint_default_fonts/fonts/{OFL,UFL}.txt`. None ships an Apache `NOTICE` file. The compat fonts are compiled into the binary with `include_bytes!` (ADR-0035), so their OFL files must be in the notices.

**Decision.**

*One read path: `ProjectFiles` (contract in `re_core`, WP19 PR "files").*
- `ProjectFiles` must sit below every crate that reads project files, and `re_app` is above them, so it goes in `re_core`, a maintainer-owned WP0 crate. That makes it a frozen-contract change the maintainer reviews, and the plan's "Package layout" row gains a second location: `crates/re_core/src/files.rs`, frozen at PR "files". The archive format itself stays in `re_app/src/package.rs`.
- `re_core::files` gains `ProjectFiles`: an ordered list of read-only layers plus the game's writable folders. Every runtime read of a project file goes through it, with project-relative `/` paths (`ProjectPath`, which refuses absolute paths, `..`, empty segments and backslashes; the checks the crates now repeat move there). It answers `read(path)`, `is_file`, `is_dir` and `list(dir, suffix)`. A read returns the bytes and their **form**: `Source`, `Bytecode { compiler }` or `Processed { importer, version }`. `read_source(path)` refuses the other two forms.
- **Layers, first match wins:**
  - first, in every run, an engine layer that serves only `.real/defaults/**` (the compat default scripts) from the constants `re_compat` already compiles in, so a compat boot no longer writes them;
  - then, in a development run (`real run`, `verify`, the editor, MCP): the project directory, as today;
  - or, in a packaged game: the archive, then the folder that holds it (the plan's lookup order: archive first, then the directory). The folder layer serves `Source` only.
- **The folder layer keeps today's checks.** It refuses a path whose canonical form leaves the folder (the symlink check `ProjectRequirer` does today, now for every read), and `real build` refuses to pack a symlink that leaves the project. Callers that build paths from other paths (a glTF's external URIs) normalise them against the referring file and get `ProjectPath`'s refusal if the result leaves the project. `material.rs`'s walk up to `real.toml` is replaced by the `ProjectFiles` it is given.
- **Only development runs write project files.** Creating a missing sidecar, following a rename and `save_scene` happen only when the directory layer is a project (`ProjectFiles::project_dir()` is `Some`). In a packaged game a loose asset without a sidecar is an error (`RE0606`), not a write, and `save_scene` returns `RE0606`. `real build` creates missing sidecars in the project before packing, as a development run already does, and lists them in its report so they get committed.
- **Writable folders.** `ProjectFiles::writable(Cache | Data | Crash | Out)`. In a development run they are `<project>/.real/cache`, `.real/data`, `.real/crash` and `.real/out`, unchanged; `Out` holds the captures runtime commands write. In a packaged game they are per user, under the package `identifier`: cache in `$XDG_CACHE_HOME` (default `~/.cache`), `~/Library/Caches` or `%LOCALAPPDATA%`; data, crash reports and `out` in `$XDG_DATA_HOME` (default `~/.local/share`), `~/Library/Application Support` or `%APPDATA%`. They come from environment variables with the standard defaults; no crate is added. `--data-dir DIR` overrides both, for portable installs and tests. ADR-0048's `.real/data/` maps to `Data`.
  - **Amendment (ADR-0049 §6, 2026-10-20).** `writable(kind)` returns a `WritableDir` handle (`read`, `write`, `remove`, `list` on project-style relative paths) instead of a `Path`. On desktop it wraps the folders above and `WritableDir::path()` gives the `Path` to tools; on the web it is backed by IndexedDB or memory and `path()` is `None`. It froze with the rest of `ProjectFiles` at WP19 PR "files" (#95), in this shape, after WP29 PR "portable" had merged.
- **Bytecode only from the archive.** The folder layer never returns `Bytecode`, and loaders set `ChunkMode` from the form instead of letting mlua sniff the bytes: `Text` for `Source`, `Binary` for `Bytecode`. A `.luau` file that holds bytecode is then a syntax error everywhere, which also closes that door in development runs.
- **Converting the read sites is one PR**, WP19 PR "files", before PR "archive". Most of it is mechanical (`std::fs::read(root.join(p))` becomes `files.read(p)`); the exceptions are the ones above (the require symlink check, glTF URIs, the material root walk, sidecar creation and renames), plus `re_compat`'s `read_to_string` becoming a byte read so bytecode can reach `load_chunk`. The maintainer reviews it with each owner asked: `re_assets`, `re_app` and `re_core` (maintainer), `re_script` and `re_proc` (WP24), `re_audio` (WP22) and `re_compat` (WP27).
- **One compiler.** `re_script` gains a public `compile_bytecode(source, chunk_name)` and `COMPILER_ID` (`"o1d1"`; `"o1d1-m"` since ADR-0036's amendment), today's `pub(crate) compiler()` behind them (WP24 reviews). `re_compat::runtime::load_chunk` sets that compiler explicitly instead of relying on mlua's default (WP27 reviews), so native and compat bytecode cannot drift apart. The hot-reload watcher keeps watching the directory layer only. Replay files and screenshots stay plain paths: they are inputs and outputs of a run, not project content.

*The archive (`re_app/src/package.rs`, frozen at PR "archive").*
- One file, `game.rpak`. Every integer is little-endian.
  - A 72-byte header: the magic `RPAK\r\n\x1a\n` (the `\r\n` and `\x1a` catch text-mode transfers, as PNG's do), `format_version: u32` = 1, `flags: u32` = 0, then `index_offset`, `index_stored_size` and `index_size` as `u64`, and the blake3 hash of the stored index (32 bytes).
  - The entries' bytes, back to back, in index order.
  - The index: postcard, zstd-compressed. `PackageIndex { engine, luau, bytecode_version, compiler, package: PackageInfo, entries }`, where `engine` is the engine version, `compiler` names the Luau compiler options (`"o1d1"`; `"o1d1-m"` since ADR-0036's amendment), and `PackageInfo` holds the name, identifier and version. `entries` is sorted by path: `IndexEntry { path, form, offset, stored_size, size, compression, hash }`, with `compression` either `None` or `Zstd` and `hash` the blake3 of the uncompressed bytes.
- **Reading.** The archive is opened once. Reads are positional (`FileExt::read_at` on Unix, `seek_read` on Windows), so threads read concurrently without a shared cursor and a truncated file is an error, not a fault (no mmap). The index is decoded at open, and paths are found by binary search. Every read checks the entry's hash. A path that misses exactly but matches one entry ignoring ASCII case is served with warning `RE0603` naming both spellings, because development runs on Windows and macOS ignore case and a game that worked there must not fail when packaged.
- **Versions.** The runtime refuses an archive whose `engine`, `luau`, `bytecode_version` or `compiler` differs from its own (`RE0602`, "rebuild the package"), because the archive and the binary come from one `real build` and bytecode must match the VM that loads it.
- **What goes in.** By default, the project's runtime content: `real.toml`, `.luaurc`, and `scenes/`, `prefabs/`, `materials/`, `graphs/`, `scripts/` and `assets/` (plus the directories later WPs add, such as WP21's animation graphs; a WP that adds a content directory adds it to this list in its PR). `[package] exclude` and `include` adjust it with `*` and `**` patterns. Never packed: `tests/`, `.real/`, `AGENTS.md` and `CLAUDE.md`.
  - **Scripts** are stored as `Bytecode` under their source path, compiled with `re_script::compile_bytecode`. A compile error fails the build with the script's `RE3001`. `[package] scripts = "source"` stores source instead, compiled at load as in development.
  - **Assets** are stored processed or as source, per asset, by `[package] assets = "auto"` (the default), `"processed"` or `"source"`. `auto` stores the processed entry when its compressed size is at most twice the compressed source (with its sidecar and dependent files), and otherwise the source, which the runtime imports at first load into the per-user cache, where later boots hit. An importer whose import is too slow to repeat at load (WP20's texture encoder) marks itself so, and `auto` always stores its output processed. Measured: the Fox and the Avocado are stored processed; the DamagedHelmet as source (3.4 MB instead of 19 MB, 0.6 s more on first boot only).
  - Sidecars, scenes, materials, graphs and `real.toml` are stored as they are.
- **Compression:** zstd level 9, per entry, kept only when it saves at least 10%. Already-compressed data (JPEG and PNG inside a `.glb`, for example) is stored raw.
- **Reproducible.** Entries are sorted, nothing records a time or a machine path, and compression is single-threaded, so the same inputs give a byte-identical archive. PR "archive" tests this.
- **Trust.** The hashes detect corruption and truncated downloads; they are not a signature. Whoever can change the archive can change the executable beside it, so the archive is trusted as the executable is. That is why bytecode comes only from the archive.

*`real build` (`re_cli/src/build/`, options frozen at PR "archive").*
- `real build` and `real verify --package` are tooling commands, behind `re_cli`'s `tooling` feature (ADR-0027): they run cargo, resize images and read crate sources, none of which a game needs.
- `real build [--project DIR] --out DIR [--target TRIPLE] [--runtime PATH] [--archive-only] [--json]`.
  1. Loads the project and runs `verify`'s content and compile stages. Any error stops the build.
  2. Collects and compiles the scripts (rayon), processes every asset through `AssetServer` with the project's `.real/cache`, picks each asset's stored form, and writes `game.rpak` through a temporary file and a rename.
  3. Gets the runtime binary. By default it runs `cargo build --profile package --locked -p re_cli --no-default-features --features game --target <triple> --target-dir target/package` in the engine workspace. The workspace gains `[profile.package]`, which inherits `release` with `strip = "debuginfo"`, so crash backtraces keep function names (47.7 MB on Linux against 37.2 MB fully stripped); thin LTO was measured and saved nothing (37.3 MB stripped). The separate target directory means the `real` that is running is never overwritten (Windows cannot replace a running executable). With `--target` always explicit, Windows target flags (`+crt-static`) reach only the target, not build scripts. `--runtime PATH` uses a prebuilt binary instead; `real build` checks it with `<runtime> --runtime-info` (engine, Luau and bytecode versions, compiler options, target) and refuses a mismatch (`RE0953`). Outside the engine workspace without `--runtime`, the build stops with a hint; shipping prebuilt runtimes with releases is a later step.
  4. Lays out the bundle for the target's OS (below) and writes `THIRD_PARTY_NOTICES.txt`.
  5. `--archive-only` stops after step 2. `--json` prints the build report: every entry with its form, sizes and compression, the stored form chosen for each asset and why, the notices, and the diagnostics.
- `--out` must be empty, missing, or a folder an earlier `real build` made (it leaves a `.real-build` marker). Anything else is refused, so a wrong path never deletes someone's files.
- **Cross builds.** The archive does not depend on the platform. Bundle layouts can be written for any target, but the runtime binary for another OS needs `--runtime`, because cross-compiling Jolt, Luau and the platform SDKs is not set up. CI builds each platform on its own runner.

*The game binary (`re_cli`, a maintainer-reviewed PR within "archive").*
- A new `re_cli` feature, `game`, adds no dependency. With it, `main` finds `game.rpak` beside the executable (on macOS, in `../Resources/`, through `current_exe`), opens `ProjectFiles` on it and runs the game. It parses only the game options: `--headless`, `--frames N`, `--replay FILE`, `--screenshot FILE`, `--json`, `--data-dir DIR`, `--version`, `--notices` (prints the bundled notices) and `--runtime-info`. Without an archive it exits with `RE0604`. It also sets `#![windows_subsystem = "windows"]`, so double-clicking the game opens no console. A parent that pipes the game's output (CI and `real verify --package`) still gets it. Started from a terminal, a GUI-subsystem program has no console, so on Windows game mode first calls `AttachConsole(ATTACH_PARENT_PROCESS)`; `--version`, `--notices` and `--runtime-info` then print there. That uses `windows-sys` 0.61.2, already in the Windows runtime graph (through clap), as a new direct dependency of `re_cli` on Windows only. PR "archive" checks this on the Windows runner.
- `cargo xtask check-graph` adds `--features game` to its runtime check. The feature adds no crate other than that Windows-only `windows-sys`, which is already in the graph, so the check means the same thing.
- **ADR-0027 amendment.** ADR-0027 says the runtime-only `real` runs, verifies, describes and explains a project. That stays true of `--no-default-features`. A binary built with `game` is a different program: it runs only the game in its archive. The amendment records the `game` feature and adds `build` to the subcommands behind `tooling`.

*Bundles (PR "bundles").* The executable is named after `[project] name`, limited to `A–Z a–z 0–9 _ -`.
- **Linux** (`--out DIR` is the game folder): `<name>` (mode 755), `game.rpak`, `<name>.png`, `<name>.desktop` and `THIRD_PARTY_NOTICES.txt`. The `.desktop` file is a template for an installer or a distribution package: `Exec` and `Icon` hold the bare names, because a desktop entry needs absolute paths that only installation knows. The docs say to ship a `.tar.gz`, since zip loses the executable bit. Packaged Linux games are built on the oldest supported runner (Ubuntu 22.04, glibc 2.35), and the docs list the system libraries above.
- **Windows:** `<name>.exe`, `game.rpak` and `THIRD_PARTY_NOTICES.txt`. The runtime is built with `-C target-feature=+crt-static`, so no Visual C++ redistributable should be needed; PR "bundles" checks the DLL imports on the Windows runner (`dumpbin /dependents`), including Jolt's CMake build. The Windows `build-package` job also runs the packaged game on DX12 (WARP) with `WGPU_DX12_COMPILER=fxc`, so the shaders are known to compile under the compiler every Windows machine has; whether the renderer should name FXC instead of `Auto` is WP20's call. The `.exe` has no custom icon in M3, because embedding one needs a resource step at link time; the window icon is set at run time instead.
- **macOS:** `<display name>.app/Contents/` with `Info.plist`, `MacOS/<name>`, and `Resources/game.rpak`, `<name>.icns` and `THIRD_PARTY_NOTICES.txt`. `Info.plist` is written as text with `CFBundleExecutable`, `CFBundleIdentifier`, `CFBundleName`, `CFBundleDisplayName`, `CFBundlePackageType` (`APPL`), `CFBundleShortVersionString` and `CFBundleVersion`, `CFBundleIconFile`, `CFBundleInfoDictionaryVersion` (`6.0`), `LSMinimumSystemVersion` and `NSHighResolutionCapable`. The `.icns` is written directly: PNG entries at 16 to 1024 px, resized from the 1024 px icon with `image` (already a workspace crate, newly a direct dependency of `re_cli`). When `real build` runs on macOS it seals the bundle ad hoc (`codesign --force --sign - <app>`), so the app runs on the machine that built it. This departs slightly from the plan's "unsigned": an ad hoc seal carries no identity, and it is what lets the bundle's `Info.plist` and resources be checked at all. A bundle laid out on another OS (with `--runtime`) is not sealed, and the build report says so. Neither is a Developer ID signature: the docs give the developer's steps (`codesign --options runtime` with their identity, `xcrun notarytool submit`, `xcrun stapler staple`), and what an unsigned, quarantined app shows on another Mac. arm64 and x86_64 are built separately; a universal binary (`lipo`) is not in M3.
- `[package]` in `real.toml` (parsed by `re_app/src/package.rs`; `AppConfig` gains the field in the "archive" PR):
  ```toml
  [package]
  identifier = "com.example.island"   # reverse-DNS: the macOS bundle id and the user folders' name
  display_name = "Island"             # default: [project] name
  version = "1.0.0"                   # default: "0.1.0"
  icon = "assets/icon.png"            # a square PNG, at least 512 px; optional
  scripts = "bytecode"                # or "source"
  assets = "auto"                     # or "processed" / "source"
  exclude = ["assets/raw/**"]
  include = []
  notices = ["LICENSES/*.txt"]        # the game's own notices, copied into THIRD_PARTY_NOTICES.txt
  ```
  A missing `identifier` builds with `local.<name>` and warning `RE0950`, because two games sharing one would share their saves.

*Third-party notices (PR "bundles").*
- `real build` lists the runtime's crates with `cargo tree -p re_cli --no-default-features --features game -e normal --target <triple>` and finds their sources through `cargo metadata`. Both are part of cargo, so nothing is added.
- For each crate it writes the name, version, license expression and the license files: those at the package root (`LICENSE*`, `COPYING*`, `NOTICE*`, `UNLICENSE`), then those up to three levels below (`vendor/JoltPhysics/LICENSE`, `fonts/OFL.txt`).
- A crate with no license file gets the standard text of its license, with its authors as the copyright line, and is flagged in the build report. These texts sit in `re_cli/src/build/licenses/`, one per license on the allow-list. Of a dual license, the first license on the allow-list is used.
- A license outside the allow-list and ADR-0019's exceptions fails the build (`RE0954`), so `cargo deny` and packaging cannot disagree.
- The compat fonts' OFL files (ADR-0035) and the project's `[package] notices` are appended. The file is written once per target, because the crate set differs per platform.
- With `--runtime`, the notices must sit beside the binary as `THIRD_PARTY_NOTICES.txt`. Otherwise the build fails, because a binary's crate list cannot be read back from the binary.

*Checking a build in CI (PR "archive").*
- `real verify --package DIR` runs the packaged game as a child process: once with `--headless --frames 120` (boot), and once per `tests/replays/*.replay.json` of the project with `--headless --replay <file> --json`. It compares each `state_hash` with a run of the project itself and fails on a difference (`RE0956`). Test files never go into the archive.
  - Each packaged run gets a fresh `--data-dir`, and the project run starts with empty persisted data (as ADR-0048's verify mode already does), so saved data from earlier plays cannot change a hash.
  - A case-only match (`RE0603`) is an error in this check, so a reference that only works on a case-insensitive file system is caught before shipping instead of differing by platform. The `build-package` CI job runs `real build` and then this on every platform.

*Diagnostic codes.* `re_app` takes `RE0600`–`RE0619` for reading packages: `RE0600` unreadable or corrupt archive, `RE0601` entry hash mismatch, `RE0602` version or compiler mismatch, `RE0603` case-only match (warning), `RE0604` no archive beside the game, `RE0605` no writable user folder, `RE0606` a write a packaged game does not allow (a missing sidecar, `save_scene`). `re_cli` takes `RE0950`–`RE0969` for `real build`: `RE0950` invalid `[package]`, `RE0951` the build stopped on errors, `RE0952` two project paths differ only by case, `RE0953` runtime build or check failed, `RE0954` notices, `RE0955` a bundle step failed, `RE0956` packaged run differs from the project, `RE0957` a packed JSON file (a scene, prefab, material or graph) names an existing project file the archive leaves out (the hint names `[package] include`). Each gets an explain page.

*Dependencies.* `zstd = { version = "=0.14.2", default-features = false }` in `re_app`, which re-exports what `re_cli` needs. Its DEPENDENCIES row and lockfile entries (`zstd-safe` 8.1.0, `zstd-sys` 2.1.1+zstd.1.5.7) come with PR "archive", along with `windows-sys` (Windows only) as a new direct dependency of `re_cli`; `image` becomes one in PR "bundles". Both are already in the lockfile. `cargo xtask dupes` shows no new duplicate.

*PR order.* "files" (the read path, cross-owner, maintainer-reviewed), then "archive" (the format, `real build --archive-only` and full builds of the Linux folder, the `game` feature, `real verify --package`, the `build-package` CI job on Linux), then "bundles" (the Windows and macOS layouts, icons, notices, and `build-package` on all three platforms). The plan names two PRs; "files" is added because the read path crosses four owners and should be reviewed apart from the format.

*Where this departs from the plan* (accepted by the maintainer):
- The plan's archive holds "the processed assets from `.real/cache`". `assets = "auto"` stores some assets as source, because processed textures are 5× bigger until WP20's encoder; `assets = "processed"` gives the plan's behaviour.
- The runtime binary is built with the new `game` feature as well as `--no-default-features` (ADR-0027 amendment above).
- macOS bundles are sealed ad hoc when built on a Mac, not left unsigned.
- `ProjectFiles` lives in `re_core`, and PR "files" is added.

**Alternatives.**
- **Loose files in a folder, no archive.** The simplest, and the second layer keeps it possible. Rejected as the format: thousands of small files install and scan slowly (antivirus on Windows), there is nowhere to record a stored form or a hash, and a case-insensitive lookup would need a separate index.
- **zip** (the `zip` crate). Standard tools can open it. But zstd entries need its optional features and a larger dependency tree, it has no per-entry strong hash, and our index must still record forms and versions next to it. The `--json` build report lists the contents instead.
- **tar.** No random access without an index of our own.
- **Appending the archive to the executable.** One file to ship, but macOS code signing expects data in `Resources/`, Windows antivirus treats modified executables with suspicion, and every rebuild rewrites a large binary.
- **A runtime compiled per game with assets in `include_bytes!`.** Every build would need a Rust toolchain and a long link.
- **Prebuilt runtimes ("export templates").** The right way to ship `real build` to people without the engine source, but it needs a release pipeline. `--runtime` is the hook for it.
- **Always processed assets.** Texture-heavy games would ship 4–6× bigger until WP20's encoder lands. **Always source.** First boot repeats every import (0.8 s for one helmet).
- **Other compressors.** deflate through miniz_oxide adds no crate, but decodes at half zstd's speed and is 12% bigger. lz4_flex decodes fastest but is 1.8× bigger. ruzstd is pure Rust but decodes 4× slower. No compression: on a fast SSD a raw read can beat decoding, but installed size grows several times.
- **mmap** (memmap2 is already in the tree). A file truncated under a mapping faults the process; positional reads give an error instead.
- **Strict case in the archive.** Rejected because a game that ran in development on Windows or macOS could then fail only once packaged.
- **Shipping source and compiling at load.** Kept as `scripts = "source"`. Bytecode is the default because it loads about 6× faster and needs no compile step at boot.

**Consequences.**
- With ADR-0047's BC7 textures, the helmet's processed textures are 12.5 MB under zstd (3.9× the source JPEGs, against 7.2× for RGBA8); textures are always stored processed, because encoding them at first launch would cost about 18 s per 100 MB of texels.
- WP23 streams cells through `ProjectFiles`, so streamed content works the same packaged and unpackaged. The index stays cheap to 100,000 entries (17 ms to decode); a world with more files would need an index loaded in pages, which is not planned.
- WP24's bake format can become a `Processed` form in the archive, so a game can ship baked generator output; until then the per-node cache is rebuilt in the per-user cache on first run.
- WP27's persistence (ADR-0048) writes through `writable(Data)` instead of joining `.real/data` itself.
- WP28 builds the valley with `real build` and checks it with `real verify --package`.
- Code that reads project files with `std::fs` after PR "files" is a bug. PR "files" adds a test that boots the island from an in-memory layer with no project directory, so a missed read site fails.
- The compat boot stops writing `.real/defaults/`. Existing copies in projects are ignored and can be deleted.
- Packaged Linux games inherit the build machine's glibc, so the Linux `build-package` job runs on Ubuntu 22.04.
- `COMPILER_ID` (`"o1d1"`, now `"o1d1-m"`: ADR-0036's amendment disables the portable-maths builtins) is the packaged compiler. Development runs compile at debug level 2 (ADR-0045), native and compat alike, so the two still match in every run; a project that asks for level 2 in its package changes `COMPILER_ID` with it.
- The `build-package` job joins ADR-0023's required checks when PR "bundles" runs it on all three platforms; the maintainer amends ADR-0023 in that PR's follow-up docs change.
- With `zstd` in the runtime, ADR-0048's reason for `ruzstd` (not building the C library) no longer holds for builds that include `re_app`. WP27 may read place-file ZSTD bodies with `zstd` instead, and drops the `ruzstd` row if it does.
- Windows `.exe` icons, installers, code signing, notarisation, universal macOS binaries, patching and the web stay out of M3.

**Amendment (WP19 PR "archive", #121; 2026-10-21).** Departures found while building it:
- The cargo profile is `[profile.packaged]`, not `package`, because cargo reserves `package` as a profile name. `real build` runs `cargo build --profile packaged --locked -p re_cli --no-default-features --features game --target <triple>`.
- `data/seed` is in the default packed content, beside the directories listed above, because a game's saved data starts from it (ADR-0048).
- Headless packaged runs do not save data, so `real verify --package` can repeat them. Windowed runs save it.
- The archive format lives in `crates/re_app/src/package/` (`archive.rs`, `config.rs`, `dirs.rs`), not one `package.rs`.
- Notices, icons and the Windows and macOS bundles stay in PR "bundles"; until then those platforms get the plain folder, and the build report says so.

**Amendment (WP21 PRs "graphs", #145, and its follow-up, #152).** `anims/` is a default packed directory, beside the ones above, for `*.animgraph.json` files. A graph elsewhere is packed when it lies in another default directory or matches `[package] include`; a packed scene or prefab that names one the archive leaves out fails the build with `RE0957`.

**Amendment (WP19 PR "bundles", #161; 2026-10-25).** Departures found while building it:
- **Runtime folders.** `real build --runtime-only [--target TRIPLE|web] --out DIR` writes a runtime and no game: `real` (`real.exe` on Windows), `runtime.json` (the `--runtime-info` versions and the target) and `THIRD_PARTY_NOTICES.txt`, or with `--target web` the web runtime's files (ADR-0049 §7). This is the "later step" of prebuilt runtimes: on a `v*` tag the release workflow publishes `real-runtime-<version>-<triple>` for `x86_64-unknown-linux-gnu` (built on Ubuntu 22.04) and `aarch64-apple-darwin` as `.tar.gz`, and for `x86_64-pc-windows-msvc` as `.zip`, beside `real-web-runtime-<version>.tar.gz`.
- **`--runtime PATH`.** When the binary cannot run here (a runtime for another OS), `real build` reads `runtime.json` beside it instead of its `--runtime-info`. Without `--target`, the build is for the runtime's own target. So a release's Windows or macOS runtime lays out its bundle on Linux; an app laid out off a Mac is not sealed, as planned. The notices come from the runtime's folder: its part, up to any earlier game's own notices, is copied, and the game's part is added.
- **The order.** `real build` checks or builds the runtime and lists its notices before it clears `--out`, so a build that fails there leaves the earlier output as it was.
- **Which crates.** `cargo tree --locked -p re_cli --no-default-features --features game -e normal,build --target <triple>`, not `-e normal`: build scripts compile C and C++ sources into the runtime, and Luau's come from `luau0-src`, a build-dependency of `mlua-sys`. Proc macros stay listed, because they write code into it. That is 310 third-party crates for Linux x86_64 today. The web runtime lists `-p re_web` the same way. The engine's own crates are not listed, but the license files under their `assets/` folders are, under "Files the engine bundles" (`re_compat`'s fonts, ADR-0035); the standard texts in `re_cli`'s source are templates, so they are not.
- **License files.** At the root and up to three levels below, as planned, with these names: `LICENSE*`, `LICENCE*`, `COPYING*`, `COPYRIGHT*`, `NOTICE*`, `UNLICENSE*`, `OFL*` and `UFL*`, any other name with `license` or `licence` in it (`emoji-icon-font-mit-license.txt`), and a `.txt` named after a font beside it (`Hack-Regular.txt`). Every file in a `LICENSES/` folder counts too, and `tests`, `test`, `benches`, `examples`, `fuzz`, `target`, `.git` and `.github` folders are skipped. Texts are compared after normalising line ends, trailing spaces and blank lines at either end, and a text several crates share is printed once, under a number the crates refer to: 223 texts, 757 KB for Linux x86_64.
- **Standard texts.** A crate with no license file at its root gets the standard text of each license it is used under, with its authors (or "the <crate> authors") as the copyright line, as planned; 19 crates on Linux x86_64 today, all named in the build report. Of a choice (`OR`, or `/` in older manifests), the notices take the alternative whose least preferred license comes first in the allow-list, then in ADR-0019's exceptions; `AND` and `WITH` keep every part. A license with no standard text needs a license file deeper in the crate, as `epaint_default_fonts` has, else `RE0954`.
- **Fonts.** A font a third-party crate ships (`.ttf`, `.otf` or `.ttc`) needs a license text in that crate: one beside it whose name starts with the font's, or one that names the font's family. Otherwise the notices print what the font's name table states (its copyright, license and license URL), with the OFL 1.1 text when it names the SIL Open Font License, and the build report lists the font; one that states none of them, or cannot be read, fails the build (`RE0954`). Today that is Noto Emoji in `epaint_default_fonts`, whose copy of the OFL does not name it, and on Linux Cantarell, which `sctk-adwaita` embeds for Wayland title bars without shipping its OFL.
- **The game's notices.** `[package] notices` files come last, under "The game's own notices", each under its project path. A pattern that matches nothing fails the build (`RE0954`).
- **Icon.** `[package] icon` must be a square PNG of at least 512 px (`RE0950`). It is packed as source even outside the default folders, because the game reads it: `re_app` sets the window's icon from it at start, shrunk to at most 256 px, and only logs a failure. On Linux it is also copied beside the game as `<name>.png`.
- **macOS.** The `.icns` entries are `icp4`, `icp5`, `ic07`, `ic08` and `ic09` (16 to 512 px) and their 2× entries `ic11`, `ic12`, `ic13`, `ic14` and `ic10` (32 to 1024 px), up to the icon's size, resized with Lanczos3. `LSMinimumSystemVersion` is 11.0 for arm64 and 10.15 for x86_64. Both version keys take up to three numbers from `[package] version` (`1.2.0-beta` gives `1.2.0`). The app's folder is the display name without the characters macOS or Windows refuse in a name, so `My: Island` makes `My Island.app`.
- **Windows.** The job does not set `WGPU_DX12_COMPILER`: `re_render` builds its instance's backend options itself and does not read it. The packaged game runs on DX12 (WARP) with wgpu's default compiler (`Auto`); whether to name FXC stays WP20's call. `dumpbin /dependents` fails the job on any `vcruntime`, `msvcp`, `ucrtbase` or `api-ms-win-crt` import.
- **CI.** `build-package` (Ubuntu 22.04), `build-package-windows` and `build-package-macos` build the island, the plaza and the valley, run `real verify --package` on each and the packaged island in a window, and check the platform: glibc and hot reload on Linux, DLL imports on Windows, `plutil -lint` and `codesign --verify --deep --strict` on macOS. Each then builds the plaza again with `--runtime` from a `--runtime-only` folder, as a developer with a release's runtime would; on Linux its notices must match the direct build's. The three jobs join ADR-0023's merge rule. The island has an identifier and a 1024 px icon, so the bundles are checked with one.

**Evidence.** `research/m3/wp19/measurements.md`: the compression table (four codecs, three models and the content text), the importer and cache-hit timings, Luau bytecode size, determinism and load time, the index timings, the runtime binary's size, `ldd` and glibc symbols, the license-file scan of the runtime crates, and the API facts checked in mlua 0.12.2, mlua-sys 0.13.0, luau0-src 0.22.0+luau740, wgpu-types 30.0.1 and zstd 0.14.2.

## ADR-0038: HDR post chain, cascaded sun shadows, shader hot reload, and no render thread yet

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP20). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: WP20's phase-0 measurements (research bundle `m3/wp20/measurements.md`, with the scratch tests that produced them).

**Context.** M3's WP20 adds HDR rendering with tone mapping and bloom, cascaded shadows for the sun, shader hot reload, and a render thread only if PerfReport shows render submission is the bottleneck (M3_PLAN, WP20). WP27 draws the compat `BloomEffect`, `BlurEffect` and `SunRaysEffect` through this chain, and WP28's valley uses it. What exists today:
- `re_render` draws the forward pass straight into the caller's target (ADR-0010). Shading is linear; lights, ambient and emissive are multiplied by `RendererSettings::exposure`; colour correction (`SceneExtras::grading`) is applied per fragment in the forward and effect shaders; the target's format does the sRGB encoding (or the shader does, for `Rgba8Unorm`-style targets). Values above 1 are clipped by the target.
- One 2048² sun shadow map is fitted each frame to the smaller of the scene's and the view frustum's bounding sphere within `shadow_distance` (60 units). There is no culling: every item is drawn in the view and every caster in the shadow pass.
- Shaders are built-in strings joined by a small `#include` preprocessor (`RE6030`).
- PerfReport's `render` phase does not include `Renderer::render`: the windowed driver draws after `App::frame` returns, so submission is not measured today.

Measured (lavapipe and llvmpipe GL, wgpu 30.0.1; measurements §1–§3):
- **Submission on the 250,000-part map** (ADR-0031's bigmap through the compat view, 1280×720, 4× MSAA): without culling, 23,347 items give 20,298 view draws plus 23,347 shadow draws, and `Renderer::render` takes **166.9 ms** of CPU a frame (p95 206 ms). With prototype culling (view frustum; casters within the shadow distance) it takes **38.6 ms** (p50 30, p95 86), about 5,500 draws mid-path. `CommandEncoder::finish` is the largest part (3–5 µs per draw). The same frames spend **1,297 ms** in `re_compat::render::build`, so submission is **2.9%** of the frame's CPU time. The plaza is not in the repository yet.
- `Rgba16Float` is renderable, blendable, filterable and 4× multisampled on both backends; `Rg11b10Ufloat` is not renderable on GL. A `Depth32Float` 2D array with one layer per cascade, rendered through single-layer views and sampled with `textureSampleCompareLevel(…, layer, ref)`, gives correct results on both. On GL, an MSAA colour texture that also has `TEXTURE_BINDING` or `COPY_SRC` usage resolves to zeros; with `RENDER_ATTACHMENT` only, it resolves correctly. GL cannot `textureLoad` a depth texture.
- A full-screen pass at 1280×720 costs about 11 ms on the software rasterisers; a 6-level bloom plus tone map about 46 ms.

**Decision.**

*Post chain (PR "post").*
- The scene pass (sky, forward, sprites, ribbons, debug lines) renders into an `Rgba16Float` target, multisampled when MSAA is on and resolved into a single-sample `Rgba16Float` texture. MSAA textures carry `RENDER_ATTACHMENT` usage only (the GL finding). `Rgba16Float` is used on every backend, so there is one path and one set of goldens; `Rg11b10Ufloat` is not used.
- The chain, in order: **sun rays** (screen-space radial blur of sky pixels from the sun's projected position, added), **bloom** (a soft-knee threshold, then a downsample chain with the 13-tap filter of Jimenez 2014 and an additive 3×3 tent upsample, mixed in by `intensity`), **blur** (separable Gaussian at half resolution), **colour grading** (today's brightness, contrast, saturation and tint, in scene-linear values), **tone mapping**, then the output encoding into the caller's target (by format, or by the shader for gamma-storing formats as today). Highlights and egui are drawn after the chain onto the final target, in display colours, as authored.
- A step whose setting is off costs nothing: no pass is recorded. The resolve and the final pass always run, which costs about 11 ms at 720p on the software rasterisers and a fraction of a millisecond on a hardware GPU; one path is worth that.
- **Tone mapping operators:** `None` (clamp; today's look), `PbrNeutral` (Khronos PBR Neutral: it removes the 4% Fresnel lift and compresses only above about 0.76, so authored base colours come out close to their values), `Aces` (the Hill fit of ACES RRT+ODT) and `AgX` (the Blender/Troy Sobotka curve, base look). **`None` is the snapshot default**, so existing scenes, goldens and compat places look as they do today. The `real new` templates and the valley set `PbrNeutral`, which is our recommended operator for new projects.
- **Exposure** keeps today's meaning (it scales lights, ambient and emissive in the shaders, so a 100,000-lux sun works at exposure 1e-5); it moves from `RendererSettings` into the snapshot. Automatic exposure is not in M3.
- **Settings join `RenderSnapshot`** in `re_types::render` (maintainer-reviewed contract change, frozen at WP20 PR "post", consumed by WP27 and WP28). Every new field has a serde default, so old snapshots and callers are unchanged:
  ```rust
  pub struct RenderSnapshot { /* existing fields */ pub post: PostSettings, pub shadows: SunShadows }
  pub struct PostSettings {
      pub exposure: f32,                  // 1.0
      pub tone_mapping: ToneMapping,      // None | PbrNeutral | Aces | AgX; default None
      pub bloom: Bloom,                   // intensity 0.0 (off), threshold 0.0, knee 0.5, radius 0.05
      pub blur_radius: f32,               // 0.0 (off); fraction of the target's height
      pub sun_rays: SunRays,              // intensity 0.0 (off), spread 0.5
      pub grading: Option<ColorGrading>,  // None; brightness, contrast, saturation, tint
  }
  ```
  `Bloom::radius` and `blur_radius` are fractions of the target's height, so a setting looks the same at any resolution (the compat layer divides `Size` in pixels by the viewport height). The bloom chain stops at the level whose texel covers `radius`, at most 8 levels. `SceneExtras::grading` and `RendererSettings::exposure` are removed from `re_render`'s API in the same PR; `re_compat` (WP27) and `re_app` (maintainer) move to the snapshot fields.
- New shader files `post.wgsl` and `tonemap.wgsl`; the grade moves out of `fx.wgsl`. With every scene pass writing linear values into the HDR target, the `*_gamma` fragment variants of the scene shaders go away; the vertex inputs and the instance layout (locations 0–11) do not change, so WP21's skinned variants stay in step.
- **Depth of field is not in M3.** Compat `DepthOfFieldEffect` stays `partial` (ADR-0048).

*Render-target textures and world quads (PR "post", for WP27's `SurfaceGui`, ADR-0048).*
- `Capture` gains `TEXTURE_BINDING` usage, and `Renderer::capture_texture(&Capture) -> TextureHandle` returns a texture handle that samples it (released with `release_texture`; the capture must outlive the handle). WP27 paints a GUI into a pooled capture with `paint_ui` and draws it with this handle.
- `RenderSnapshot` gains `quads: Vec<WorldQuad>` (default empty), part of the contract frozen at PR "post":
  ```rust
  pub struct WorldQuad {
      pub entity: EntityId,
      pub world: Mat4,             // the quad spans −0.5..0.5 in its local X and Y, facing +Z
      pub size: Vec2,
      pub texture: TextureHandle,  // premultiplied RGBA, as egui paints it
      pub depth: QuadDepth,        // Tested { bias: f32 } (in world units towards the camera) | OnTop
      pub light_influence: f32,    // 0 = unlit, 1 = lit by the scene's lights like a matte surface
      pub brightness: f32,         // multiplies the unlit part
  }
  ```
  Depth-tested quads are drawn in the HDR scene pass with the blended items, sorted back to front with them, and go through the post chain. `OnTop` quads are drawn after the chain onto the final target with no depth test, in display colours, like highlights.
- `ViewportFrame` needs nothing new: it is a second `render` into a `Capture`, shown through `register_ui_texture`.

*Codes reserved for other WPs.* `RE6040` for WP21: "the adapter lacks `VERTEX_STORAGE`; skinned items are drawn in bind pose" (ADR-0039).

*Cascaded sun shadows (PR "shadows").*
- **Up to four cascades** in one `Depth32Float` 2D array texture, one layer per cascade, each `RendererSettings::shadow_map_size` square (2048 by default, 64 MB for four). Only the sun (the one directional light with `shadows`) is shadowed, as today.
- **Splits:** the practical split scheme (Zhang et al. 2006), a blend of logarithmic and uniform splits between the camera's near plane and `shadows.distance`, weighted by `split_lambda`. Defaults: 4 cascades, distance 150 units, `split_lambda` 0.75, transition band `blend` 0.1 of each cascade's depth. `RendererSettings::shadow_distance` moves into the snapshot.
  ```rust
  pub struct SunShadows { pub cascades: u8 /* 1..=4, default 4 */, pub distance: f32 /* 150.0 */, pub split_lambda: f32 /* 0.75 */, pub blend: f32 /* 0.1 */ }
  ```
- **Stability:** each cascade is fitted to the bounding sphere of its slice of the view frustum, so its size does not change when the camera turns; the sphere's radius depends only on the field of view, aspect and split distances. The light-space centre is snapped to whole shadow-map texels, so the map does not shimmer when the camera moves. The depth range of each cascade is extended towards the sun to the bounds of the casters that project into it, so casters outside the slice still cast.
- **Sampling:** the forward shader picks the cascade by view depth, blends into the next cascade across the `blend` band, and filters with the current 3×3 hardware-compare kernel. The normal-offset bias scales with each cascade's texel size.
- **Culling arrives with cascades:** items are culled against the view frustum, and casters against each cascade's light-space box extended towards the sun, before batching, using each mesh's bounds transformed by the item's matrix. The prototype cut submission on the bigmap from 166.9 ms to 38.6 ms. Skinned items (WP21) are culled by a world-space bound that `SkinnedDrawItem` carries, because bind-pose bounds do not contain an animated pose.
- The shadow pass draws each cascade with its own light matrix, selected by a dynamic uniform offset; `shadow.wgsl`'s vertex inputs are unchanged.
- Goldens with shadows change (sharper near shadows); PR "shadows" re-blesses them with before and after images. A stability test draws a static scene from cameras along a path and requires the sun visibility at fixed world points to stay identical while the points stay in one cascade.

*Shader hot reload (PR "hot reload").*
- `Renderer::set_shader_source(name, Option<String>)` overrides (or restores) one of the built-in files of `shader::SOURCES`, or adds a new includable file. `Renderer::reload_shaders()` rebuilds every pipeline whose root file includes a changed file, directly or through includes.
- A rebuild **keeps the last good pipeline** on any failure. Errors are reported with file and line: the preprocessor records which file and line each output line came from, naga (`wgpu::naga`) parses and validates the joined source first, and `ParseError::location` / `WithSpan::location` map back through that record. New codes in `re_render`'s range: **`RE6031` shader compile error** (file, line, column, naga's message) and **`RE6032` pipeline rejected by the device** (caught with `Device::push_error_scope(ErrorFilter::Validation)` around module and pipeline creation, so no error reaches the uncaptured handler).
- **What is watched** (in `re_app`, a maintainer-reviewed change, because `re_render` does not depend on `re_assets`; ADR-0006): the project's `shaders/*.wgsl`, always in a development run; and the engine's own shader directory when `RE_ENGINE_SHADERS` names it (engine developers point it at `crates/re_render/src/shaders`). The existing `AssetWatcher` delivers the changes; a packaged or `--release`-style run loads the project's `shaders/` once at boot and does not watch.
- A project file with the same name as an engine file replaces it. This is a development tool for the engine's own shaders, not a stable API: engine shader files can change in any release, and `real verify` warns (`RE6033`) when a project replaces an engine file, naming the file.

*Render thread.*
- **Metric:** a new PerfReport field, `render_submit` (mean, p95, max), the CPU wall time of `Renderer::render`/`render_with` and `paint_ui` for a frame (preparation, encoding, `finish` and `submit`), excluding waits for the GPU (presentation and readback). `re_app` records it in the windowed driver and in captures (maintainer change, in PR "post"'s description).
- **Threshold:** a render thread is built when, after culling, on the reference container at 1280×720, `render_submit` has a mean of **at least 4 ms** and is **at least 25% of the frame's CPU time** (the frame phases plus `render_submit`), on the bigmap (250,000 parts) or the plaza. Below 4 ms (a quarter of a 60 Hz frame) the extra frame of latency and the cross-thread resource lifetimes do not pay; below a 25% share another phase is the bottleneck, and overlapping submission cannot bring the frame into budget.
- **Today's numbers and decision: no render thread.** On the bigmap, submission after culling is 38.6 ms but 2.9% of the frame; the bottleneck is the compat view build (1,297 ms a frame, `re_compat::render::build` walking every part; reported to WP27), then the software GPU. WP20 measures again on the bigmap and the plaza after PR "shadows" (culling) and after the plaza lands, and records the numbers in this ADR. If either crosses the threshold, a PR moves `Renderer::render` to a dedicated thread over the `RenderSnapshot: Send` seam (ADR-0007), with uploads and releases sent on the same channel in order. WP28 measures the valley and reopens the decision only if it crosses the threshold.

**Alternatives.**
- **`Rg11b10Ufloat` as the HDR format** (half the memory). Not renderable on GL, so goldens would need two chains; rejected for one path.
- **Tone mapping by default** (`PbrNeutral` or `AgX` in the snapshot default). Every existing golden and compat place would change look. Rejected: projects opt in, and templates opt in for them.
- **A fast path that skips the HDR target when the chain is off.** Saves one full-screen pass (~11 ms on software, negligible on hardware) at the cost of a second set of pipelines and goldens. Rejected.
- **Bloom with a hard threshold and Gaussian blur.** Flickers on small bright details; the downsample/upsample chain is stable and cheaper.
- **Fitting cascades tightly to the slice** (better texel density, unstable under rotation). Rejected for the stability requirement. **Variance or moment shadow maps:** light bleeding and more memory; PCF is enough for M3.
- **Shader hot reload by restarting the renderer.** Loses every GPU resource; rejected.
- **A render thread now.** Submission is not the bottleneck on any scene that exists, so it would add latency and complexity for no frame-time gain (ADR-0007).

**Consequences.**
- `RenderSnapshot` gains `post`, `shadows` and `quads`, frozen at WP20 PR "post" (WP27 maps the compat effects onto them; WP28 sets them for the valley). Their serde defaults keep old scene files and agents' snapshots valid.
- `re_render`'s public API loses `SceneExtras::grading`, `RendererSettings::exposure` and `RendererSettings::shadow_distance`; `shadow_map_size` becomes per cascade. `re_compat` and `re_app` move in the same PRs (their owners review).
- GPU memory grows by the HDR targets (at 1920×1080 with 4× MSAA: 66 MB multisampled plus 16.6 MB resolved, plus about 6 MB of bloom levels) and the cascade array (64 MB at 2048²).
- Goldens for tone mapping, bloom, cascades (both backends) are added; goldens with shadows are re-blessed once in PR "shadows".
- `RE6031`–`RE6033` are added with explain pages.
- A packaged game reads the project's `shaders/` through `ProjectFiles` once at boot (ADR-0037) and never watches; `RE6033` is a `real verify` warning, so it never blocks a packaged run.
- PerfReport gains `render_submit`, which makes later render-thread decisions measurable in CI and by agents.

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
- `re_types::render` gains `SkinnedDrawItem { entity, mesh, material, world, flags, palette: u32, joints: u32, bounds: Aabb }` (`bounds` is world-space) and two `RenderSnapshot` fields with serde defaults: `skinned: Vec<SkinnedDrawItem>` and `joint_palette: Vec<Mat4>`. `palette` is the item's first matrix in `joint_palette`, so one buffer holds every character's palette and `DrawItem` stays `Copy` and unchanged. A palette matrix is `joint_model * inverse_bind` in the mesh's model space; `world` places the mesh as for a static item. The snapshot literals in `re_render`, `re_app`, `re_compat` and the `re_types` tests that do not end in `..Default::default()` get it in the "skins" PR; WP20's "post" PR adds fields to the same struct, so whichever lands first converts them.

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
- When the adapter lacks `VERTEX_STORAGE`, the renderer warns once with `RE6040` (allocated by WP20; the "skins" PR adds the constant and its explain page). Skinned pipelines use only the linear fragment entry points, after WP20's "post" removes the gamma variants (ADR-0038). It then draws skinned items in their bind pose (palette set to identity). It does not fake animation. No M3 target lacks the flag.
- `SkinnedDrawItem.bounds` is a world-space `Aabb`: the union of each joint's bind-space vertex bounds moved by its posed joint matrix and the item's `world`, computed in `re_anim`. It is conservative, and WP20's frustum culling (PR "shadows", ADR-0038) culls skinned items with it.

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

**Amendment (WP21 PR "graphs", #145).** As built:
- The root joint that root motion reads is joint 0. The importer makes it the skin's `skeleton` node when that is a joint with no parent in the skin, and otherwise the first root joint in depth-then-file order. `GLTF_IMPORTER_VERSION` is 3, so cached imports rebuild.
- Additive nodes and additive layers give no root motion. A clip node's `speed` must not be 0 (pause a graph with `Animator.playing` or `Animator.speed`).
- `HostServices` gains five required methods for the `anim` library: `anim_set`, `anim_get`, `anim_trigger`, `anim_state` and `anim_joint`.
- Graphs step, and root motion moves the entity, in PreAnimation. `BoneAttachment`s are placed at the end of PostSimulation, after the ragdoll blend, so `PostSimulation` scripts see the previous step's placement. When several graphs play, they step in parallel on the task pool.
- For every bound graph, playing or paused (a paused graph's parameters still change what it plays next), `App::state_hash` adds each graph's playing flag, speed, parameters, states and times, in entity order, to the hash (see ADR-0036's amendment); poses are not hashed.
- Codes: `RE7607` invalid graph (naming the element by its path in the file), `RE7608` a clip that cannot be loaded, `RE7609` an unknown or mistyped parameter, `RE7610` a mask joint the skeleton lacks (warning) and `RE7611` a blend-tree cycle.
- `animgraph.validate` is `read` tier; `anim.set_param` changes runtime state and is not undoable. With them `real mcp` lists 53 tools. `schemas/animgraph.schema.json` is exported with the other schemas.

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
  - ranks 15 to 19, in the order `RemoveRagdoll`, `AddRagdoll`, `SetRagdollMode`, `SetRagdollTarget`, `ApplyRagdollImpulse`, after the vehicle commands; `RemoveBody` is never needed for parts, which the ragdoll owns. Every removal, the vehicle and ragdoll ones included, sorts before every other kind (`PhysicsCmd::sort_key` is `(phase, rank, entity)` with removals in phase 0), so `RemoveRagdoll` frees its entity before an `AddBody` on it in the same buffer;
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

**Amendment (WP22 PR "vehicles", #94; 2026-10-21).** The default wheel tester is `Ray`, not `CastCylinder` (`re_types::WheelTester` and the scene's `Vehicle.tester`). In the fixed-path test, a 1 µm change in start position moved the cylinder tester's end pose by 8.5 m, while the ray tester's moved by millimetres. `CastCylinder` and `CastSphere` stay selectable per vehicle. `PhysicsCmd` gains `RemoveVehicle`, `AddVehicle` and `SetVehicleInput` at ranks 12 to 14, and `HostServices` gains `vehicle_drive` and `vehicle_state`.

**Amendment (WP22 PR "ragdolls", #134).** A `Ragdoll` component on an entity with a `SkinnedMesh` builds a ragdoll on the skin's skeleton (fields `joints`, `total_mass`, `mode`, `strength`, `blend_seconds`; modes `Animated`, `Simulated` and `Powered { strength }`). `PhysicsCmd` gains `RemoveRagdoll`, `AddRagdoll`, `SetRagdollMode`, `SetRagdollTarget` and `ApplyRagdollImpulse` at ranks 15 to 19, and `HostServices` gains `ragdoll_mode`, `ragdoll_state` and `ragdoll_impulse`. An invalid ragdoll or command is `RE5019`; a skin `re_app` cannot build a ragdoll for is `RE0529`, reported once. Where it differs from the Decision above:
1. Poses cross `PhysicsCmd` in the skeleton's model space, with the entity's position and rotation carried on `AddRagdoll` and `SetRagdollTarget`, not in joint-local space. Mapping to parts needs model space, so this skips a conversion on both sides.
2. The blend uses nlerp on the short arc, not slerp, because slerp needs `acos` and `sin` (ADR-0036). Between two poses of one skeleton the difference is not visible.
3. The component has no part list. Parts are generated from the skeleton and a list of joint names (or the humanoid names when `joints` is empty), with the shapes, masses and limits `RagdollDesc::from_skeleton` derives.
4. `re_app` adds a root joint above the skin's joints, so skins with several roots work. The entity needs a uniform world scale, and model space is multiplied by it.
5. Ragdoll parts report no contact events and no body poses. Queries see a part as its entity.
6. A character and a ragdoll may share an entity, and the character ignores its own parts.
7. A change of `Powered` strength rebuilds the ragdoll, because oxijolt's ragdoll settings are immutable. The rebuild keeps each part's pose and velocity.
8. `physics.ragdoll_state` returns no pose: scripts get the root, the mode, the strength and `calm`.
9. Rotations of the mapped pose match the parts, but positions can differ by up to about 2 cm, because the mapper locks translations to keep bones their length.
10. A mode set by a script holds until the component's `mode` field is next edited.

**Amendment (WP22 ragdoll follow-ups, #143).**
- A physics snapshot keeps each ragdoll's mode, motor scale, frame and target, and `restore_state` puts them back after the Jolt state. A snapshot whose ragdolls differ (one added, removed or rebuilt since) is refused with `RE5012`.
- `App::state_hash` covers the ragdolls' physics state (ADR-0036's amendment), and run reports, `real run` and the web checkpoints print it.
- A rebuild keeps the mode physics has and the blend weight, so a mode a script set survives an edit of another field; an invalid `strength` is reported once.
- The scene enum for the component's `mode` is `RagdollControl` (it was `RagdollMode`, the same name as `re_types::physics::RagdollMode`). The scene schema has only the variant names, so files are unchanged.

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
  - `audio.set_rate(e, rate)`, which writes `e`'s `AudioSource.rate` as `e:set` does, so `NoServices` does not refuse it;
  - `audio.set_bus_volume(bus, db, seconds?)`;
  - `audio.set_effect(bus, index, { param = value, … }, seconds?)`;
  - `audio.snapshot(name, seconds?)`;
  - `audio.levels(bus) -> { peak_db, rms_db }`.
  - The `AudioSource` component in `re_scene` gains `bus: String`, empty by default, which means `default_bus`, and `rate: f32`, 1 by default (the playback speed, at most 16). The maintainer reviews this.
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

**Amendment (WP22 PR "audio", #115; 2026-10-21).** Deviations found while building it:
1. An effect's `kind` is the serde tag, so the EQ's shape field is named `shape` (`bell`, `low_shelf`, `high_shelf`) and the distortion's is named `mode` (`hard_clip`, `soft_clip`).
2. The upper bound for filter and EQ frequencies is a fixed 20 kHz, not the Nyquist frequency. The device rate is not known when the graph is validated, and 20 kHz is below Nyquist for every supported rate.
3. `audio.set_effect(bus, index, ...)` takes a 1-based `index`, the Luau convention. The `effect` index in `real.toml` snapshots and in `AudioCmd` stays 0-based.
4. A delay's `seconds` must be between 0.001 and 10 (kira panics at 0 and allocates the whole delay up front), and its feedback must be 0 dB or less.

Also: the bus volume is a `volume_control` effect, not the track volume, so meters and sends read after the fader (kira applies a track's volume after its effects). `HostServices` gains `audio_mix` (one of the four mixer commands) and `audio_level`. `StopAll` also returns the mixer to the graph's own values, so a restart starts clean. `audio.levels` measures live audio, so it is frame-only: calling it during a fixed step is an error, because the simulation would no longer replay. The five new `AudioCmd` variants are appended after every existing one.

## ADR-0042: World streaming: cells by StableKey, deterministic budgets, stitched terrain LOD, regional erosion and a floating origin

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP23). The code waits for M2's definition of done and for WP24 PR "versioning" (M3_PLAN). Evidence: WP23's phase-0 measurements (research bundle `m3/wp23/measurements.md`, with the scratch benchmark that produced them).

**Context.** M3 wants worlds larger than memory: a 16 km × 16 km generated world walked end to end, with content loading and unloading around the player, terrain level of detail, physics on the terrain, erosion, and no frame waiting on generation (M3_PLAN, WP23). The same world fully resident at 1 m would be 16,384² samples, about 12 GiB of mesh, so it has to stream. What exists today, and what it means:
- A procedural source regenerates as a whole (ADR-0017). `World::regenerate` diffs the source's entire output, so adding one 200-entity cell to a source that already has 19,800 generated entities took 136.6 ms, against 0.75 ms for the same 200 entities alone (measurements §5).
- `GenContext` borrows the `World`, so generators run on the main thread; the ProcGraph `Evaluator` itself needs no `World` and is shared behind an `Arc` (ADR-0015).
- There is one worker pool, shared with Jolt (ADR-0007), and rayon has no priorities. Generation running beside a 1000-body pile raised the mean step from 3.9–4.1 ms to 4.9–5.4 ms with two jobs in flight and 5.9–6.0 ms with three; no step stalled, because Jolt's barrier runs queued jobs on the stepping thread (§6).
- Jolt stores a heightfield quantised per shape. Two neighbouring chunks built from the same edge samples disagree by up to 9.8 mm along that edge with the default settings (8 bits, block 2) that `re_physics` uses today, and by 0.1 mm with 16 bits and block 4, which also uses less memory (10.8 against 15.8 KiB per 65² shape) (§3).
- Positions are f32 (scene transforms, Luau vectors, Jolt without double precision). Jolt refuses positions beyond ±5 km of its origin (ADR-0008); ADR-0031 already moves the compat physics origin for one static map.
- Determinism: D1 must hold (ADR-0018) and D2 is coming (ADR-0036). Anything whose timing depends on worker threads must not change simulation state at a timing-dependent tick.

**Decision.**

*Cells and their contract (PR "cells"; frozen there for WP28).*
- The world has one square grid of **cells**, aligned with world X and Z, of `[stream] cell_size` metres. A cell is named by the StableKey `c/<x>_<z>` (`c/-3_12`), where `x` and `z` are its integer grid coordinates. The key is the cell's identity everywhere: events, overrides, logs, caches and commands.
- `re_scene::stream::StreamCell` (plain data, in `re_scene` so that `re_script` can see it without depending on `re_proc`): `key: String`, `coord: [i32; 2]`, `min: Vec3`, `max: Vec3` (world bounds; Y from the content), `sources: Vec<String>` (the stable ids of the streamed sources with content in it) and `version: String` (the hex of `re_proc::version_stamp(graph, registry)` from WP24's "versioning", ADR-0043: the generator's name and version plus each node's id, kind, recorded version and implementation hash).
- A **streamed procedural source** is an ordinary source (ADR-0017) with a `Streamed` component and an identity transform (anything else is `RE4302`). Its graph is evaluated per cell instead of once (see "Windows" below). Every entity it generates in a cell has a key under the cell, `c/<x>_<z>/<node id>/<local key>`, so overrides, deletions, suppressions and commands address it exactly as ADR-0017 does.
- An **authored** root entity with `Streamed` belongs to the cell holding its translation at load. While its cell is unloaded the subtree lives as `EntityDoc`s (the scene format) in a per-world store, so saving writes it and nothing is lost; during play, unloading keeps the subtree's current state (`World::snapshot_subtree`) and reloading restores it. References from outside a streamed subtree into it read as empty while it is unloaded and are reported once (`RE4303`).
- **Focus.** Entities with a `StreamFocus` component are the focus points (the player, a vehicle, the editor camera); with none, the active camera is. The *desired set* is every cell whose bounds come within `radius` of a focus, plus, ahead of the focus (its forward within ±60°), out to `radius_ahead`. A loaded cell unloads only when every focus is farther than `radius_ahead + unload_margin` (hysteresis), never because the focus turned (amended in PR "cells": with `radius`, a cell loaded ahead would unload as soon as the focus turned).
- **Events.** Scripts see `stream.on_loaded(function(cell) … end)` and `stream.on_unloaded(…)`, where `cell` is the `StreamCell` as a table, plus `stream.cells()` and `stream.is_loaded(key)`. Events fire in `PreSimulation`, before scripts' fixed update, unloads first, each list in key order. `real.toml` budgets and the events are the frozen part; the Luau binding lives in `re_script` (WP24 reviews).
- **Overrides survive unloading.** Overrides stay on the source (ADR-0017) and are re-applied when the cell reloads. An override whose target sits in an unloaded cell is not an orphan; `RE4010` is reported only for loaded cells. Suppressions (play state) also survive unload and reload within a session. Other play state of *generated* entities (a rock a vehicle pushed) is rebuilt from the generator on reload; a game that needs it kept uses an authored entity.
- **Addressing unloaded entities.** A command, script or agent naming an entity in an unloaded cell gets `RE4304` with the cell key and a hint (`stream.load` in tools contexts and the editor, which load a cell explicitly and pin it until released).

*`real.toml`* (in `re_proc::stream::StreamConfig`; `re_app`'s `AppConfig` gains `[stream]`, maintainer-reviewed):

```toml
[stream]
cell_size = 256.0          # metres per cell side
radius = 768.0             # cells within this distance of a focus load
radius_ahead = 1280.0      # and out to this, within ±60° of the focus's forward
unload_margin = 128.0      # hysteresis before unloading
memory_mb = 1024           # resident streamed data (accounted, see below)
workers = 0                # generation jobs in flight; 0 = pool threads − 1, at least 1
delay_ticks = 30           # a request made at tick t is applied at tick t + delay_ticks
apply_entities = 400       # streamed entities spawned or despawned per tick, at most
apply_chunks = 8           # terrain collision chunks added or removed per tick, at most
terrain_view = 6000.0      # terrain LOD horizon, metres
physics_radius = 256.0     # terrain collision chunks within this distance of a focus
origin_grid = 1024.0       # the floating origin snaps to this grid
origin_shift = 2048.0      # and moves when a focus is this far from it on X or Z
```

An unknown key or a bad value is `RE4300` at load, like the other sections.

*Determinism and budgets.*
- **Everything that changes simulation state happens at ticks computed from simulation state.** The desired set, priorities and cancellations are computed at each fixed step from the focus transforms. A cell requested at tick `t` is applied at tick `t + delay_ticks`, never earlier, in key order, within the per-tick apply budget (`apply_entities`, `apply_chunks`; work beyond it moves to the next tick, deterministically). Budgets are counts, not milliseconds, because a wall-clock budget would make the applied set depend on the machine.
- If a result is not ready at its tick: in play, it is applied when it arrives and warning `RE4305` (late cell) is logged once per cell; in `real verify`, the D2 harness and replays, the frame **waits** for it, and the report counts these waits. "No frame waits on generation" is checked as *zero waits* in the acceptance walk. With waits counted rather than hidden, a replay gives the same `state_hash` on any machine, slow or fast.
- **Memory** is accounted, not sampled: each cell's bytes are the sum of its meshes (vertex and index bytes, counted once for the CPU copy and once for the GPU), heightfield samples, physics shapes (by the measured per-shape size), stored documents and a fixed estimate per entity. When the total passes `memory_mb`, cells no focus wants are evicted first, farthest first; if that is not enough, the effective radius (and the ahead reach with it) shrinks and `RE4306` is reported once. The radius grows back one cell at a time once accounted memory is under 3/4 of the budget (amended in PR "cells"). Accounting from data sizes keeps eviction deterministic. The process RSS is reported beside it but decides nothing.
- **Workers.** At most `workers` generation jobs are in flight (default: pool threads − 1, so 2 of 3 in the reference container). Jobs are issued from the main thread at tick boundaries, never respawned from a worker (a worker's own `spawn` goes to its local LIFO deque, which measured worst), and a streaming job evaluates its nodes serially: no fan-out inside a job. Measured cost to physics with two jobs in flight: +25–35 % mean step on a 1000-body pile; the walk measures it on streamed terrain.

*Off-thread generation and cancellation (PR "async").*
- On the main thread, a request snapshots its inputs: the source's graph with its parameter overrides applied, its seed, the cell or chunk window, and the generator-version fingerprint. The job runs on the pool with the shared `Evaluator` and returns `re_proc` values (fragments, meshes, fields). Turning a fragment into a `SceneFragment` and applying it happen on the main thread at the apply tick, through `World::apply_cell` (below).
- Each job carries a cancel flag. The evaluator checks it between nodes, and streamable nodes check it between row batches through `NodeCtx` (WP24 reviews). A pending or running request is cancelled when its cell leaves the desired set before it is applied (the focus turned away from a cell that was only wanted ahead, or moved on), when its source's fingerprint changes (the request is reissued with the new inputs), and when the world closes. A cancelled job's partial node outputs are not cached.
- Results are cached by the per-node keys of ADR-0015, which carry WP24's versions and the window, in memory and in `.real/cache/proc/`: a cell walked past twice is generated once per version, and there is no separate cell cache to invalidate.

*Windows: how a graph is evaluated per cell or chunk (needs WP24's node set).*
- WP24 adds the fields in its PR "versioning" (ADR-0043), so `NodeSpec` freezes once; their semantics and the value for each existing kind come in WP23's PRs, reviewed by WP24. `NodeCtx` gains `domain: Option<Domain>`, `Domain { origin: [i64; 2], n: u32, spacing: f32 }`: a window of `n × n` samples whose sample `(0, 0)` is the global sample `origin` at `spacing` metres. Every streamable node computes world positions as `global index as f32 * spacing`. Measured: the samples one LOD level shares with the next are then bit-identical (12,960 checks per spacing at 1.0, 0.7 and 2.5 m), because doubling a float is exact (§1). The domain is part of each node's key.
- Each node kind declares in `NodeSpec.streaming` (default: not streamable) how it streams: **pointwise** (`terrain.heightfield`, `field.combine`, `field.remap`: output window = input window), **regional** (`terrain.erode`: region tiles with an apron, below), **cell-local** (`scatter.*`, `points.*`, `points.instance`, `terrain.water_plane`: run on the cell's window with a per-cell seed `fork2(x, z)` of the node seed; points belong to the cell that contains them; keys under `c/<x>_<z>/`), or **not streamable** (`terrain.island_mask`, which is relative to the field's size). A streamed source whose graph uses a node that does not stream is refused with `RE4301`, naming the node. Poisson scatter is per cell, so two points either side of a cell border can be closer than its radius; `scatter.grid_jitter` is exactly seamless. This is documented on the node.

*Terrain (PR "terrain"; the terrain module `re_geom::terrain`, agreed with WP24).*
- **Height function.** A streamed graph's terrain is the field reaching its `terrain.chunks` node (new, WP23's, registered through WP24's `NodeRegistry`). Because every node evaluates at exact global positions, the terrain is one function of world position, and any window of it agrees with any other on shared samples.
- **Level of detail: a chunked quadtree.** Every chunk at every level is 65 × 65 samples (64 cells); level `L` has spacing `base_spacing × 2^L`. Root tiles are fixed-size squares of level `max_level` (default 5: 2048 m at 1 m base spacing), loaded within `terrain_view`. A chunk splits when a focus is nearer than 1.5 × its size, and the selection is balanced so neighbours differ by at most one level. Measured size along the 16 km walk at a 6 km horizon: at most 264 chunks, 76 MiB of mesh on the CPU and as much on the GPU (§7). Normals come from a window one sample wider than the chunk, so shading matches across chunk borders.
- **Stitched seams.** Where a chunk's neighbour is one level coarser, that edge's triangles skip its odd vertices (one fan per coarse cell), so the fine edge is made of exactly the coarse edge's vertices, which are bit-identical by construction. There are 16 edge patterns; a chunk's mesh is rebuilt off-thread when its pattern changes (indices only; 0.25 ms per 65² chunk including normals). No skirts, no tolerance.
- **Render chunks are not entities.** LOD chunks are view data, not simulation state: they live in a `TerrainView` resource (meshes as `Derived` assets keyed by chunk and pattern), are swapped in as soon as they are ready and in budget by time, and `re_app`'s render sync draws them with the source's material and the source's `EntityId` (so picking selects the source). They are left out of `state_hash`.
- **Physics heightfields per chunk.** Level-0 chunks within `physics_radius` of a focus are generated entities, keys `t/<x>_<z>` under the source, with a heightfield `Collider` made from the same 65² samples, so they flow through the existing derived-heightfield path in `re_app` and appear in `state_hash` at deterministic ticks. They use **16 bits per sample and block size 4**, which closes collision seams to 0.1 mm (§3): `ShapeDesc::Heightfield` gains an optional `precision` field whose default keeps today's Jolt defaults, so existing projects and replays are unchanged (a frozen `re_types` addition, maintainer-reviewed; `re_physics` passes it to `HeightFieldSettings::bits_per_sample` and `block_size`, WP22 reviews). Cost per chunk: 0.4 ms to build the shape, 10.8 KiB resident.
- **Hydraulic erosion: `terrain.erode`.** Droplet erosion (Beyer 2015) in `re_geom::terrain::erode`, with only `+ − × ÷` and `sqrt`, no fused multiply-add and a `Pcg32` per tile, so it is bit-identical everywhere (`re_noise`'s rules). It runs on **region tiles** of 256 × 256 cells at its own spacing (default 4 m, so 1024 m tiles) plus an apron of 32 samples each side. The input field is evaluated over the tile and apron; each tile erodes independently with seed `fork2(tile x, tile z)`; the eroded height at a point is a blend of the tiles whose cores and aprons cover it, with weights that depend only on the point's global position, so the result is one continuous function of position with no tile seams. Finer windows interpolate the eroded grid bilinearly at their exact positions, so every LOD level and every collision chunk still agrees on shared samples. Measured: 190 ms per 1 km tile without apron on one thread (about 2.9 µs per sample; §4). Tiles are cached on disk by key, and the terrain within the horizon is not shown until the tiles it covers are ready: a cold start of the 16 km walk erodes the tiles inside 6 km (about 130, each about 0.3 s with its apron) in roughly 13 s on three workers, behind the load screen; afterwards new tiles come in at the horizon, kilometres ahead of the focus.

*Floating origin for native scenes (with PR "terrain").*
- The simulation origin `O` lies on the `origin_grid` (1024 m) and moves, at a tick boundary before the tick's physics commands, when any focus is more than `origin_shift` (2048 m) from it on X or Z. It follows the focus deterministically, from simulation state, so D1 and D2 hold. With several focuses, `O` is the grid point nearest their mean, and focuses that would end up beyond Jolt's ±5 km get `RE4308`.
- `re_physics::Physics::set_origin` is extended from part assemblies (ADR-0031) to every body and character, through oxijolt's `PhysicsWorld::rebase`, which moves bodies, characters and gravity in one rigid change and checks everything before writing (oxijolt 1.1.0, `world/rebase.rs`). WP22 reviews.
- Scene transforms stay world-space f32: within ±8 km a position is exact to 0.98 mm (the f32 spacing at 8192 m), which covers the 16 km world. `re_app` builds the render snapshot camera-relative (the camera position is subtracted in f64 before matrices go to the GPU), so precision near the camera does not depend on where the camera is (maintainer reviews). Content beyond ±16 km gets warning `RE4309`; worlds larger than that need f64 or cell-relative transforms, which M3 does not plan.

*Scene hooks (`re_scene`, maintainer reviews).*
- `World::apply_cell(source, cell_key, fragment)`: the ADR-0017 diff restricted to generated keys under `cell_key`, with a per-source ordered key index so its cost is proportional to the cell, not the source (the measured 0.75 ms per 200 entities, not 136 ms).
- `World::unload_cell(source, cell_key)`: despawns those entities without recording deletion overrides or suppressions, and marks the cell unloaded for orphan reporting.
- The authored-document store for unloaded `Streamed` subtrees, which `World::save` includes.

*Diagnostics* (`RE4300`–`RE4499`, inside `re_proc`'s range): `RE4300` invalid `[stream]` settings; `RE4301` a node that does not stream in a streamed graph; `RE4302` a streamed source with a non-identity transform; `RE4303` a reference into an unloaded streamed subtree; `RE4304` an entity in an unloaded cell; `RE4305` a cell applied late; `RE4306` memory budget cannot hold the radius; `RE4307` a cell's generation failed (the cell stays empty and is retried when its inputs change); `RE4308` the origin cannot cover every focus; `RE4309` content beyond the precision limit. Each gets an explain page.

*Budgets this ADR sets for the acceptance walk* (`stream_walk`, the 16 km × 16 km generated world with erosion, scatter and terrain collision, focus moving at 30 m/s corner to corner, reference container, headless, no rendering):
- zero frames waiting on generation, after the initial load;
- tick time (simulation, streaming bookkeeping and apply) p95 ≤ 8 ms and max ≤ 16.7 ms (see the amendment below: the max is main-thread CPU time);
- accounted streamed memory ≤ `memory_mb` (1024 MiB) and peak RSS ≤ 1536 MB;
- the same `state_hash` on two runs, from a cold and from a warm cache.
PR "async" measures these on the walk; if a budget cannot be met, the PR changes a default with its measurement and amends this ADR rather than relaxing the check.

**Alternatives.**
- **Regenerate the whole source when a cell changes.** It reuses ADR-0017 as is, but costs in proportion to everything loaded: 136.6 ms to add one cell to 19,800 entities.
- **Apply results as soon as they are ready.** Simplest, and no frame ever waits, but the loaded set at a given tick then depends on thread timing: two runs of the same replay would differ, breaking D1 and D2.
- **A wall-clock apply budget (milliseconds per frame).** Same problem: the applied set would depend on the machine. Counts are deterministic; the milliseconds they cost are measured and reported.
- **A separate streaming thread pool.** Gives priorities, but breaks ADR-0007's one pool and oversubscribes four cores. Capping the jobs in flight at threads − 1 measured acceptable, and Jolt's barrier keeps physics progressing on the stepping thread.
- **Fanning a streaming job's rows over the pool** (as `par_rows` does today). Lower latency per chunk, but it occupies every worker at once; serial jobs are enough (about 1,100 chunks of 129² in 600 steps with two in flight).
- **Skirts instead of stitching.** Hide cracks whatever the heights, but show at grazing angles and cost overdraw; with bit-identical shared samples, stitching is exact.
- **Geomorphing CDLOD or GPU clipmaps.** Smoother transitions, but they need vertex-shader work in `re_render` (WP20). Possible later, behind `TerrainView`.
- **Jolt's default heightfield precision.** 9.8 mm steps at chunk borders, measured; 16 bits with block 4 costs less memory.
- **Erosion per chunk.** Cheap, but each chunk erodes differently at its border, and LOD levels would disagree. **Erosion of the whole world up front.** Simple and seamless, but 49 s on one thread for 16 km, and it does not scale to larger worlds; region tiles with blended aprons give the same seamlessness lazily.
- **One scene file per cell for authored content.** Keeps authored data out of memory, but adds a project layout and an editor workflow. Documents in memory cost little next to generated content; cell files can come later if a project needs them.
- **Keeping the play state of generated entities across unload.** It needs a per-entity snapshot store for content that can be rebuilt; deferred until a game needs it.
- **f64 transforms.** The real fix beyond 16 km, but it touches every subsystem and contract; the floating origin and camera-relative rendering cover the 16 km world.

**Consequences.**
- No new dependencies: rayon (through `TaskPool`), oxijolt, blake3 and postcard are already in the workspace; channels and cancel flags are std.
- Changes in other owners' paths, each a small PR reviewed by the owner: `re_scene` hooks and the `StreamCell` type (maintainer); `[stream]` in `re_app`'s config, camera-relative render sync and `TerrainView` drawing (maintainer); `ShapeDesc::Heightfield` precision (maintainer, frozen-contract addition with a default) and its use in `re_physics`, plus the general `set_origin` (WP22); `Domain`, streamability and cancellation in `NodeCtx`/`NodeSpec`, the `terrain.erode` and `terrain.chunks` registrations, the `stream` Luau binding in `re_script` (WP24).
- WP23's own paths: `re_proc/src/stream/` (the manager, requests, budgets, accounting, events, config) and `re_geom/src/terrain/` (quadtree selection and balancing, stitch patterns, chunk meshes with apron normals, erosion and tile blending), all pure computation testable without a GPU.
- Physics on streamed terrain costs a few static bodies (about 50 level-0 chunks within 256 m), not one per cell of the world.
- Generation competes with physics for cores while cells load; the cap keeps it bounded, and PerfReport shows streaming as its own row.
- A slow machine in play sees late cells (`RE4305`), never a hitch; in verify it waits and the wait is counted, which is the check.
- The plan's three PRs map onto this: "cells" (contract, config, `World` hooks, synchronous generation at the deterministic tick), "async" (jobs, cancellation, the walk and its budgets), "terrain" (LOD, stitching, collision chunks, erosion, floating origin).

**Amendment (WP23 PR "async", #128; maintainer decision, 2026-10-21).**
- **The tick max gates main-thread CPU time.** "Tick max ≤ 16.7 ms" is checked on the largest per-tick main-thread CPU time of a run (`CLOCK_THREAD_CPUTIME_ID` on Linux). Each run's raw wall-clock max and wall p99 are reported beside it, not gated, because shared CI runners take the CPU away at random for longer than a tick. The wall p95 budget and zero waits are unchanged, so blocking is still caught. A platform without a thread CPU clock reports the wall max, ungated.
- **Cancellation acts between node levels.** The evaluator checks a job's cancel flag before each level of the graph, not inside a node, so a cancelled cell finishes the node it is running (a Luau node may run until its own budget). A cancelled job reports `RE4310` inside the job; it never reaches the user, and the job is issued again with the current inputs. A node that panics fails its cell with `RE4307` instead of the process.
- **Lateness is set explicitly.** `Lateness::Wait` is the default: `real verify`, `real run --replay`, `play.run` copies and the D2 harness wait for a late result and count it. Only a live windowed `real run` calls `App::set_stream_lateness(Lateness::Late)`, where a late cell applies when ready and is reported once as `RE4305`. Play copies (`Streamer::forked`) start in Wait mode with no jobs carried over.
- The WP28 valley's walk was re-measured after this PR: no tick generates a cell inline.

**Amendment (WP24 PR "streamed roads", #159; 2026-10-24).** A fifth class, **global**, for shapes and road paths: one result for every window, keyed without it, which samples its fields on the source's grid instead of taking them over the window. ADR-0043's amendment of the same PR has the rules and the `RE4301` checks. Cell, terrain-chunk and collision-chunk jobs pass the source's spacing as `EvalOptions::global_spacing`, so roads drape and flatten the same at every level. When a source has terrain chunks, the cells' graph (`split_terrain`) also drops shape and path nodes whose output nothing uses, as it drops field and points nodes. A region node reads each input from that input's own subgraph.

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

**Amendment (WP24 PR "versioning", #93; 2026-10-21).**
- The warning for an open graph that differs from its file is `RE4022`, because `RE4021` is bake's.
- The `.heightfield` layout adds the f32 x, y and z of sample (0, 0) after the spacing, so a baked collider keeps its offset: `REHF`, version, `n`, spacing, offset, then the samples.
- `RE4019` is reported in the graph generator's evaluation report (`graph.report`, load diagnostics) and in the migration report (`graph.migration_report`, a read command), not by `real check`, which does not read graphs.

**Amendment (WP24 PR "bake", #105; 2026-10-21).**
- Every bake, full or partial, despawns the baked keys' generated entities in its own transaction. That records their deletion overrides, and the inverse restores them exactly, so no separate deletion-override step is needed. A full bake then clears the source's overrides (last first) and removes the generator's parameter component and `Procedural`.
- Baked asset directories use the encoded source id: `assets/baked/<encoded source id>/`, where anything outside `[A-Za-z0-9_-]` becomes `%XX` and Windows device names are escaped, so the name is portable.
- `RE4021` covers everything that cannot be baked: not a source, a streamed source, no output, an empty or unknown key, a key whose generated parent is not baked, or an asset that cannot be written.
- Bake is not refused during play: `play.run` and editor play run on a copy of the world, so there is no live play state for a command to check.
- The `re_scene` rule as built: a source that has a fingerprint but no `Procedural` has its output despawned and its fingerprint dropped at the next regeneration; re-adding `Procedural` regenerates the same ids.
- Not yet: `List<Entity>` references are not remapped to baked ids, and `real check` does not yet warn about unreferenced files under `assets/baked/`.

**Amendment (WP24 PR "luau", #125; 2026-10-21).** An out-of-memory error that a `pcall`, `xpcall` or `coroutine.resume` catches still fails the node (`RE4101`). A generator script that fails to declare on reload keeps the last good kind, if there was one. `proc.create_source` refusals are `RE4104`. The generator VM installs ADR-0036's portable maths (`re_script::portable_math`), like every other VM.

**Amendment (WP24 PR "booleans and roads", #142).**
- A road's footprint is the union of its ribbon's triangles, so a curve tighter than half the road's width still gives the area it covers. `road.mesh` overlaps itself on such curves; control points should keep curves wider than that.
- A junction forms where centrelines of different paths cross, or where an open end comes within `snap` of another path. A path that crosses itself makes no junction. Junction finding compares every segment of each pair of paths, after a bounding-box test, so its cost is quadratic in the segment count.
- Polygons use the positive fill rule: a hole that sticks out of its outer contour, or overlaps another hole, only removes area.
- `NodeRegistry::m1()` registers 28 kinds, WP23's two terrain kinds included. The disk cache stores shapes and paths as new variants after the M1 ones, with no format version change. Luau nodes take and return both (`re_script::generator::marshal`).

**Amendment (WP24 PR "streamed roads", #159; 2026-10-24).** Roads and shapes stream (ADR-0042), so the valley (WP28) can have roads on its eroded, streamed terrain.
- **A global class.** `Streaming::Global` marks a node whose output does not depend on the window: `path.spline`, `road.footprint`, `shape.polygon`, `shape.boolean` and `shape.offset`. In a streamed evaluation a global node runs without a window. Its key hashes its inputs' keys as they are without a window, plus the global spacing, so every cell and chunk of a source shares one result through the memory and disk caches. Windows evaluated at the same moment on a cold cache may each run it and get the same result; that costs CPU while a world loads, not correctness. A global node outputs shapes or a path. When not streamed, every key and output is unchanged.
- **Fields are sampled, not windowed.** A global node does not get its fields over the window. It asks for them where it needs them (`NodeCtx::sample`), interpolated on the triangles of the source's grid at `EvalOptions::global_spacing`. That spacing is the source's for cells and chunks alike, so a chunk at 8 m sees the road a 4 m cell sees. The samples come from small windows of the field's own subgraph, one per 16 × 16 grid cells with points in them, through the same caches. `path.spline` drapes its control points this way. A node whose output only reaches global nodes' fields does not run over the window (`Skipped`).
- **Pointwise and cell nodes.** `terrain.conform` and `field.from_shapes` are pointwise: each sample is tested against the whole of every path or shape, so windows agree on shared samples. `road.mesh`, `shape.mesh` and `points.mask` work per cell. A cell node's mesh is local to the window's corner (`min x, 0, min z`), where its entity is placed, and an empty mesh makes no entity.
- **Meshes per cell.** `road::network` joins the paths once, and `Network::mesh_in` keeps the triangles whose centre lies in the cell's half-open square, with the whole mesh's normals and UVs. The cells' parts add up to the whole road exactly, and `Network::mesh` is byte-identical to the old `road_mesh`. `shape.mesh` triangulates the shapes once (`polygon::triangles`), and `Triangles::mesh_in` cuts each triangle at the cell's sides. A point where a triangle's edge crosses a side is worked out from that edge and side alone, and a point the cut already put on the side (where an edge crossed an x side at the cell's corner) is kept as it is. So neighbouring cells put their shared border's points in the same places, bit for bit, with no gap, overlap or T-junction. UVs stay in world metres.
- **Shared values.** The joined network and the triangulation are kept in the evaluator's memory cache by content hash (`NodeCtx::shared`), counted against its byte budget and dropped least recently used first, so any number of networks are each joined once while used. A cell asking while another cell makes the value waits for it. `EvalOptions::global_spacing` must be positive; a global node sampling at any other spacing fails.
- **Refused graphs** (`re_proc::nodes::check_streaming`, run when a streamed source is prepared; `RE4301`):
  - a global node outputs something other than shapes or a path;
  - shapes or a path come from a node that is not global;
  - a global node takes a field from a node that is not pointwise or region, or another input from a node that is not global;
  - a node upstream of a global node is not global, pointwise or region.
- **Junctions without the quadratic search.** Junctions are found through a uniform grid of segment boxes, which finds exactly what comparing every pair found (tested on random networks).

## ADR-0044: Editor node canvas, docking and gizmos: egui-snarl, egui_dock and our own gizmos; graph files become open documents edited by `graph.*` commands

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP25). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). The maintainer chose our own gizmos over transform-gizmo-egui (below).

**Context.**
- WP25 (M3_PLAN) adds a node canvas for ProcGraph, translate/rotate/scale gizmos and dockable panels to `re_editor`. Every edit must be a command, so it is undoable and agents see the same changes (ADR-0003).
- The engine pins egui, egui-wgpu and egui-winit `=0.36.2` and glam `=0.34.1` (ADR-0028). No second egui is ever accepted (DEPENDENCIES, `deny.toml`).
- Checked on crates.io on 2026-10-15, by reading each crate's published source and by building the chosen crates together in a scratch crate on egui 0.36.2 (one egui in the tree):

  | Crate | Version (date) | egui | License | Fit |
  |---|---|---|---|---|
  | egui-snarl | 0.12.0 (2026-08-26) | `^0.36` | MIT OR Apache-2.0 | Port and pin node editor. Deps: ahash, egui-scale (243 lines, same author), slab, smallvec; serde optional. No unsafe. |
  | egui_node_editor | 0.11.0 (2026-08-21) | `^0.36` | MIT | Fork of egui_node_graph, small user base; applies each edit itself and reports it afterwards. |
  | egui_node_graph / egui_node_graph2 / egui_nodes / egui_graph | yanked / 0.7.0 / 0.1.3 / 0.16.5 | none on 0.36 (0.29, 0.14, 0.34) | MIT | Not usable. |
  | egui_graphs | 0.32.0 | `^0.36` | MIT | A graph visualiser (petgraph), no ports. Not a node editor. |
  | egui_dock | 0.21.1 (2026-08-06) | `^0.36` | MIT | Tabs, splits, floating windows; `DockState` serialises with the `serde` feature. Deps: duplicate, paste, thiserror. `forbid(unsafe_code)`. |
  | egui_tiles | 0.17.1 (2026-08-18) | `^0.36.0` | MIT OR Apache-2.0 | Tabs and splits, no floating windows; serde on by default. |
  | transform-gizmo-egui | 0.11.0 (2026-08-17) | `^0.36` | MIT OR Apache-2.0 | Translate, rotate, scale and arcball gizmos; its core crate uses glam 0.32 internally and mint at its API. |

- The workspace MSRV is Rust 1.95 (`rust-version` in the root `Cargo.toml`). transform-gizmo-egui 0.10 and 0.11 declare Rust 1.96, and 0.9 is built on egui 0.34.
- Graph structure today lives only in files (`graphs/*.graph.json`, a `GraphDoc`). The `graph.*` commands set per-source overrides (`graph.set_param`) or regenerate; none adds a node or an edge. File writes such as `script.write` are not in undo history (checkpoints cover them). So "every canvas edit is an undoable command" needs a way to edit a graph file inside a transaction.
- `GraphDoc` has no node positions, and `NodeDoc` is `deny_unknown_fields`. WP24's per-node cache key will hash node contents, so layout must not be part of a node.

**Decision.**
- **Crates.** Add two workspace dependencies, used only by `re_editor` (a tooling crate, ADR-0027):
  - `egui-snarl = "=0.12.0"` with `serde` for the node canvas;
  - `egui_dock = "=0.21.1"` with `serde` for docking;
  - Gizmos are our own code in `re_editor`, on glam 0.34 only (see **Gizmos**). transform-gizmo-egui is not taken: its current versions need Rust 1.96, above the workspace MSRV, and it would bring a third glam (0.32.1) plus enum_dispatch and enumset.
  - Other new crates in the tree: egui-scale 0.6.1 (canvas); duplicate 2.0.1 and proc-macro2-diagnostics 0.10.1 (docking, compile-time). ahash, slab, smallvec, paste, heck and thiserror 2 are already in the lockfile. Each new crate gets a DEPENDENCIES row.
- **Graph files are open documents.** A graph file the editor or an agent edits is loaded into the World as an open document, edited in memory, and written back by the ordinary project save, as scenes are.
  - A new operation joins the closed set (ADR-0003): `set_doc {doc, pointer, value?}` adds, replaces or removes the JSON value at a JSON-pointer path in an open document (`doc` is a project-relative path) and returns its exact inverse. A document kind registers a validator by suffix (`.graph.json` → `GraphDoc::validate`, registered by `re_proc`), and a transaction whose result does not validate is rejected and rolled back, like any failing op.
  - `re_proc`'s `GraphStore` reads an open document in preference to the file, so regeneration sees unsaved edits. Unsaved documents are reported by `project.status` and saved by `scene.save`.
  - The undo, redo, dry-run and log machinery is unchanged: `set_doc` is one more op.
- **New `graph.*` commands** in `re_proc` (WP24's crate; WP25 writes them, WP24 reviews). Each is one transaction of `set_doc` ops, so the canvas, `real do` and MCP agents share them:
  - `graph.add_node {graph, kind, id?, params?, pos?}`: checks the kind and parameters against the `NodeRegistry` and records the kind's current version (ADR-0043); a fresh id is generated and logged when omitted (as `entity.spawn` does).
  - `graph.remove_node {graph, node}`: removes the node, its edges and its layout entry in one transaction.
  - `graph.connect {graph, from: [node, port], to: [node, port]}`: checks that the ports exist and their types match; connecting to an input that already has an edge replaces that edge, in the same transaction.
  - `graph.disconnect {graph, from, to}`.
  - `graph.move_nodes {graph, positions: {node: [x, y]}}`: one command per finished drag, however many nodes moved.
  - `graph.set_default {graph, node, param, value}`: the parameter value in the graph file. `graph.set_param` keeps meaning a per-source override.
- **Layout is not part of a node.** `GraphDoc` gets an optional top-level `layout: {"<node id>": [x, y]}`, omitted when empty and excluded from generation, from every node cache key and from the graph-document hash in a source's fingerprint (ADR-0043), so moving a node never regenerates anything. The canvas lays out nodes that have no entry (by topological depth) and writes an entry only when a node is moved, so agent-built graphs need no positions. This changes the `GraphDoc` format, so it lands with or before WP24 PR "versioning" and is agreed with WP24.
- **The canvas never edits the document itself.** The editor rebuilds a `Snarl<NodeRef>` view-model from the open document whenever the document's revision changes and treats it as read-only:
  - `SnarlViewer::connect`, `disconnect`, `drop_inputs` and `drop_outputs` are overridden to queue commands instead of changing the snarl;
  - nodes are added and removed only through our menus, which queue `graph.add_node` and `graph.remove_node`;
  - three snarl paths change the snarl without asking the viewer (node dragging, Ctrl-drag re-routing from a connected pin, and the collapse toggle; `src/ui.rs` lines 1388, 1509, 1669 and 2423 in 0.12.0). After each `show()` the editor diffs the snarl's wires against the document and turns differences into commands; positions are snapshotted when a drag starts and sent as one `graph.move_nodes` when it ends; collapse state is view state, kept in the editor layout, not in the document;
  - a test drives each interaction and checks that the document equals the result of the logged commands, so a snarl upgrade that adds another direct edit fails CI.
- **Docking.** Panels (game view, hierarchy, inspector, console, graph parameters, graph canvas per open graph, and later the debugger) are tabs in an `egui_dock::DockState<PanelId>`, where `PanelId` is a serde enum. The layout is UI state, not project content: no command, no undo, saved to `.real/editor/layout.json` (ignored by version control, like the rest of `.real/` except the agent policy) when it changes and on exit. A missing file gives the default layout; a file that does not parse, or names an unknown panel, gives the default layout and a warning diagnostic in the editor's range (`RE88xx`).
- **Gizmos.** The gizmo works on the selected entities' world transforms (several at once). During a drag the entity moves live: the editor sends a `scene.apply` of `set_field` ops on the entity's local `Transform` (world-to-local conversion through the parent) whenever the drag changed, at most every 50 ms (`LIVE_INTERVAL`), and once more on release. Every edit of one drag carries the same per-drag `coalesce` key, so the history merges them (the entity-core track's coalescing, #133) and the drag is one undo step; physics, scripts, procedural children, the inspector and the log see it as it happens. Escape puts the entity back by undoing the drag's step (amended by #140). A generated entity's edit records an override, as every `set_field` on one does (ADR-0017). The gizmo is our own (`re_editor/src/gizmo/`): translate arrows and planes, rotate rings including a view-axis ring, scale handles, picking by screen-space distance to each handle, local or world orientation and snapping, with the drag maths unit-tested without a window. Its settings are editor UI state, kept with the layout.

**Alternatives.**
- **egui_node_editor** for the canvas. On egui 0.36, but it adds edges, removes nodes and moves nodes itself before reporting them, so every edit would have to be undone and replayed as a command, and its user base is small.
- **Our own node canvas.** No dependency, full control over the command flow, but pan, zoom, wire hit-testing, pin layout and menus are a few thousand lines; snarl's three direct edits are cheaper to diff than to rewrite.
- **egui_tiles** for docking. Dual-licensed and well maintained, but no floating windows, which the debugger and a second graph canvas want.
- **transform-gizmo-egui** for gizmos. It has the picking and drag maths already, but 0.10 and 0.11 need Rust 1.96 (the workspace MSRV is 1.95), 0.9 is on egui 0.34, and it brings a third glam (0.32.1, behind mint), enum_dispatch and enumset. Our own gizmos are a few hundred lines. It can be reconsidered when the MSRV moves to 1.96 or later.
- **Node positions inside `NodeDoc`.** Simpler to read, but moving a node would change its cache key and invalidate generation; it would also need `NodeDoc`'s deny-unknown-fields format to change.
- **A sidecar layout file** (`graphs/island.layout.json` or under `.real/`). Keeps `GraphDoc` untouched, but positions get lost when a graph is renamed or copied, and an agent would have to edit two files.
- **Write graph files directly from the canvas** and rely on checkpoints. Not undoable step by step, and agents could not see or dry-run canvas edits.
- **A typed `graph_edit` op** instead of `set_doc`. Clearer in the log, but `re_command` cannot depend on `re_proc`, and WP25 also edits `AnimGraph` documents (WP21's contract); `set_doc` with a validator per document kind covers both.

**Consequences.**
- `re_command::Op` gains `set_doc` and the World gains open documents: a change to a maintainer-owned WP0 contract, made in a maintainer PR (entity-core track) and reviewed by WP24 and WP25.
- `GraphDoc` gains `layout` (a maintainer frozen-contract change in `re_types`). WP24 agreed: "versioning" makes `re_proc`'s document hash and `version_stamp` ignore it; if the field lands after "versioning", the WP25 PR adds that skip for WP24 to review. `ProcGraph::to_doc` and `from_doc` carry `layout` through unchanged, so a parameter round trip keeps it.
- `real migrate-graph` rewrites files, so the editor saves or discards an open graph document before a migration (`graph.migration_report` flags a source whose open document differs from its file).
- The hierarchy panel maps a drag of a generated entity to `proc.move` (ADR-0043), not `reparent`.
- `re_editor` gains two dependencies and three new transitive crates, all MIT or MIT OR Apache-2.0; none ships in the runtime binary, because `re_editor` is linked only with `re_cli`'s `tooling` feature (ADR-0027). No new duplicate versions.
- egui-snarl is pinned exactly; an upgrade must re-check the list of direct edits, and the canvas test catches a new one.
- A layout saved by a newer editor with a panel this editor does not know falls back to the default layout rather than failing.

**Amendment (as built, #91, #113, #123, #140).** Docking and gizmos landed in #91: the panels are dockable tabs, the layout is saved in `.real/editor/layout.json`, a file that does not parse is `RE8804` with the default layout, and W, E and R pick the translate, rotate and scale gizmo. The open documents and `set_doc` landed in #113, and the canvas with the six `graph.*` commands in #123. The canvas adds only egui-snarl and egui-scale to the lockfile; duplicate and proc-macro2-diagnostics, listed above, are not pulled in. Live gizmo edits landed in #140. Where the app writes edits back to the scene file (`AppConfig::persist_edits`), each live edit rewrites the file, so very large scenes may later want the write throttled or deferred to the end of the drag.

## ADR-0045: The script debugger: Luau's own breakpoints, stepping on temporary breakpoints, pausing by yield

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP25). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). The spike shows the full debugger can be built at no measurable cost when no breakpoint is hit, so WP25 builds it (editor panel and DAP) rather than the "stack and locals on error" fallback.

**Context.**
- WP25 (M3_PLAN) asks for breakpoints, step, locals and the call stack, in the editor and over the Debug Adapter Protocol (DAP), if a spike shows an acceptable cost; otherwise a stack and locals dump on error. ROADMAP's premise was that Luau has no `set_hook`, so a debugger would have to sit on the interrupt callback.
- Luau does have a debugger interface, and mlua-sys 0.13.0 (under mlua 0.12.2, Luau 0.740) exposes all of it: `lua_breakpoint(L, funcindex, line, enabled)`, `lua_singlestep`, `lua_stackdepth`, `lua_getinfo`, `lua_getlocal`, `lua_break`, and the `debugbreak`, `debugstep`, `debuginterrupt` and `debugprotectederror` callbacks in `lua_Callbacks`.
  - `lua_breakpoint` patches the opcode byte of the first instruction of a line into `LOP_BREAK`, in that function and every function nested in it, snapping to the next line with code. It keeps the original opcodes aside, and the `LOP_BREAK` handler calls `debugbreak` and then runs the original instruction. Nothing else in the interpreter changes.
  - `lua_singlestep` makes the next entry into the interpreter use its single-step variant, which calls `debugstep` before every instruction. The interpreter variant is chosen when it is entered, so turning single-step on inside a callback does not affect the code already running.
- The spike (WP25 research bundle: `research/m3/wp25/debugger-spike/`, a crate on the engine's exact mlua, 14 functional tests and a benchmark) tried both approaches.

**What the spike found.**
- **Interrupt-based breakpoints do not work for straight-line code.** The interrupt runs only at safepoints: calls, returns and loop back edges. In the fixture, breakpoints on lines 2, 6, 8, 10 and 11 were seen only on 8 (a call) and 11 (a return); lines 2, 6 and 10 were never seen. Checking lines at every safepoint also costs +20% to +183% in instructions executed while any breakpoint is set, even when it is never hit.
- **Native breakpoints work, with full stack and locals.** A breakpoint on straight-line code stops on each pass with the right locals (`a`, `b` per call); the stack shows every caller with its line; breakpoints snap to the next line with code (lines 4, 9 and 12 snap to 5, 10 and 13; a line past the end is refused).
  - Local names need **debug level 2**. The engine compiles at debug level 1, which keeps lines and function names but no local names, and folds a constant local away (`local label = "done"` has no instruction at level 1, so a breakpoint on its line snaps to the next one).
- **Stepping.**
  - Step over and step out work on temporary breakpoints: every line of every Luau function on the stack is patched; a stop counts only in the same frame (same depth and closure) or a shallower one (step out: shallower only). Stepping over a 3-iteration loop from line 6 stops on 6, 7, 8, 8, 8, 10, 11 and never inside the called function. In a generated 4,005-line chunk a step over or out takes 2 to 4 ms.
  - Step into on temporary breakpoints has to patch every line of every loaded chunk. That costs about 100 ms per step in the 4,005-line chunk (98 to 131 ms over three runs), since each `lua_breakpoint` call walks the whole function tree.
  - Step into by single-step works when the thread is suspended (below): turn single-step on, resume, and stop at the first instruction on a new line or depth. Measured cost per step: under 0.01 ms.
- **Three ways to pause, and which work through mlua.**
  - **Synchronous**: the handler runs inside `debugbreak` and the script waits for it. Works in every context, including inside `require`, metamethods and `table.sort` comparators, but it blocks the thread that runs the VM, so the editor's window would freeze.
  - **Yield**: `debugbreak` calls `lua_yield(L, 0)`. mlua's `Thread::resume` sees an ordinary yield of no values, and the thread resumes later where it stopped. The resumed thread re-executes its `LOP_BREAK`, so the debugger skips that one hit. Possible only where the thread could `coroutine.yield`; elsewhere the spike falls back to synchronous (2 of 2 stops in a `table.sort` comparator).
  - **`lua_break`**: `lua_resume` returns `LUA_BREAK` (6), which mlua 0.12.2 does not know: `Thread::resume` panics with "mlua internal error: unrecognized lua error code". Not usable without bypassing mlua's resume.
  - After a yield from a hook, `lua_getinfo` on the suspended thread reports level 0 one instruction early (line 7 instead of 10 in the fixture), because the saved pc points at the `LOP_BREAK` itself. The debugger reads the stack inside the hook, before yielding.
- **The CPU budget.** A synchronous pause counts against the script's CPU budget unless excluded: a 60 ms pause under a 40 ms budget stopped the script with "script exceeded its time budget"; moving the deadline by the pause time (as `re_script::untimed` does) let it finish.
- **Cost when no breakpoint is hit.** Four Luau workloads (recursive calls, a numeric loop, tables and strings, vector entity updates), each with the engine's budget interrupt installed. Instructions executed under callgrind, relative to the engine's current setup (debug level 1, budget interrupt):

  | Configuration | calls | numeric loop | tables and strings | entity update |
  |---|---:|---:|---:|---:|
  | debug level 2 | +0.00% | +0.00% | −0.03% | +0.00% |
  | native: debugger attached, no breakpoints | +0.01% | +0.00% | −0.01% | +0.00% |
  | native: a breakpoint set, never hit | +0.01% | +0.00% | −0.01% | +0.00% |
  | interrupt: debugger check, nothing set | +1.85% | +2.20% | +0.21% | +0.72% |
  | interrupt: a breakpoint set, never hit | +154.66% | +183.31% | +19.68% | +59.96% |
  | single-step mode on | +288.83% | +317.77% | +41.45% | +232.43% |

  The native rows execute the same interpreter path as today; the 0.01% is the session's setup. Wall-clock medians (15 runs, three runs, about ±10% noise on the 4-vCPU test machine) agree: the native rows fall between −11% and +16% with no consistent sign, the armed interrupt costs +6% to +198%, and single-step +19% to +287%.
- A breakpoint that is hit and continued at once, reading the whole stack and its locals each time, costs about 1 µs per hit (921 to 1,157 ns over three runs).
- Debug level 2 makes bytecode 5% to 11% larger (the five example scripts: 4,394 to 4,735 bytes).

**Decision.**
- **Build the full debugger**: breakpoints, step over, into and out, the call stack, locals and upvalues, in the editor and over DAP. It lives in `re_script/src/debug/` (WP25), with codes in `re_script`'s `RE3xxx` range and editor codes in `RE8800`–`RE8899`.
- **Breakpoints are Luau's own** (`lua_breakpoint` and `debugbreak`), not the interrupt callback.
  - Every chunk a context loads is registered with the debugger by its project-relative path (a registry reference to the chunk's function), so a breakpoint by file and line reaches every function in it. That needs a one-line call at each place a chunk is loaded: `re_script`'s module and script loaders (WP24's paths in M3) and `re_compat`'s `load_chunk` (WP27). Those are small PRs in their owners' paths.
  - A hot-reloaded chunk is a new function: the debugger re-applies the file's breakpoints when it is registered.
  - Scripts are compiled at **debug level 2** in development runs (`real run`, `real editor`, `real verify`, tests). Packaged games keep level 1 unless the project asks (WP19 decides its default).
  - With the `jit` feature, native code ignores breakpoints, so a debugging session compiles its contexts without native code.
- **Stepping.** Step over and step out use temporary breakpoints over the functions on the stack, filtered by depth and closure. Step into uses single-step on the suspended thread; where the stop is synchronous it uses temporary breakpoints over every registered chunk. Every temporary breakpoint is cleared at the next stop, and the user's breakpoints on the same lines are put back.
- **Pausing.**
  - Where the stopped thread can yield (script threads, signal handlers, `task.spawn`, `task.defer` and `task.delay`, which all run in scheduler threads), the debugger yields it with no values. The scheduler parks a thread the debugger stopped instead of treating it as a bare `coroutine.yield`; this is a one-line check in `scheduler.rs` (WP24's path, a small PR). The runtime then reports itself paused, and the app stops stepping the simulation after the current frame while the editor keeps drawing. Step and continue resume that thread through the scheduler at once; continue also unpauses the app.
  - Where it cannot yield (inside `require`, a builder script, a metamethod or a Rust callback), the stop is **synchronous**: it waits inside the hook while a DAP client answers. The CPU budget's deadline moves by the time spent paused. In the editor with no DAP client attached, a synchronous stop would freeze the window with no way to continue, so instead the debugger records the stack and locals in the debugger panel, says why it did not stop (a warning in `RE3xxx`), and continues.
  - A pause request ("pause" in the editor or DAP) is checked in the budget interrupt where it already reads the clock, every 1,024 safepoints, so it adds nothing to the safepoint path.
- **Values are read without running Luau.** Locals, upvalues and table entries are read with raw stack and table access (`lua_getlocal`, `lua_getupvalue`, `lua_rawget`/`lua_next`). `__tostring` and `__index` are never called while paused; a userdata shows its type name and, for engine types, the fields the binding exposes.
- **DAP.** `real run --debug [port]` and the editor's debugger setting listen on `127.0.0.1` only (default port 4711) and speak DAP over TCP: `initialize`, `attach`, `setBreakpoints`, `configurationDone`, `threads`, `stackTrace`, `scopes`, `variables`, `continue`, `next`, `stepIn`, `stepOut`, `pause` and `disconnect`, with `stopped`, `continued`, `output` and `terminated` events. The protocol's JSON is written with serde_json, which is already a dependency; no new crate. Threads in DAP are script contexts (game, tools, compat server and client). The editor's debugger panel uses the same session API in process.
- **Not in scope:** conditional breakpoints and logpoints (DAP lets an adapter decline them; they are a follow-up), evaluating expressions while paused (it would run Luau on a paused VM), data breakpoints, and a VS Code extension (the adapter is a TCP server; editors with a generic DAP client such as Neovim's nvim-dap attach directly).

**Alternatives.**
- **The interrupt callback** (the M3 plan's premise). Rejected: it cannot stop on straight-line code at all, and while any breakpoint is set it costs up to +183% in instructions executed.
- **Single-step mode for every session.** Rejected: +41% to +318% in instructions for the whole session, against nothing measurable for patched breakpoints.
- **`lua_break` instead of a yield.** Rejected for now: mlua 0.12.2 panics on the `LUA_BREAK` status. Using it would mean resuming script threads with raw `lua_resume` in the scheduler, duplicating mlua's argument and error handling.
- **Synchronous pauses only.** Simpler and works everywhere, but it freezes the editor window on every breakpoint, and the editor cannot pump its window events from inside a frame (winit runs the event loop).
- **Only a stack and locals dump on error** (the plan's fallback). It is a subset of this design. A script thread that died with an error keeps its frames, and the spike read them with the same code (`x = 5`, `y = 10` in the failing function, `n` and `total` in its caller), so the runtime's error report in `real run` and `real verify` will carry locals as well.

**Consequences.**
- Development builds compile scripts at debug level 2: no measurable runtime cost, bytecode 5% to 11% larger.
- `re_script` gains `src/debug/` and its unsafe FFI calls, alongside the budget interrupt's (ADR-0011): each call is commented with why it is sound, and the debug module is tested with the fixtures from the spike.
- Three one-line hooks land in other owners' paths: chunk registration in `re_script`'s loaders and `re_compat`'s `load_chunk`, and the stopped-thread check in `scheduler.rs`.
- A breakpoint inside `require` or a metamethod does not stop in the editor unless a DAP client is attached; it shows the stack instead.
- Step into from a synchronous stop costs about 100 ms per step in a 4,000-line chunk.
- If a later mlua version handles `LUA_BREAK`, the yield can be swapped for `lua_break` inside `re_script/src/debug/` without changing the scheduler hook.

**Amendment (WP27 PR "debug hooks", #146).** Compat Scripts and ModuleScripts register their compiled chunks with `re_script::debug` under their source paths, in `re_compat`'s `runtime/scripts.rs` (not a `load_chunk` function). When a script stops or restarts, or a module leaves the cache on reload, its old chunk is unregistered, and the new one registered, so breakpoints keep working after a hot reload.

**Amendment (WP27 follow-up, #154): implemented.** The app attaches the debugger to a compat project's `server` and `client` contexts as well as `game`: before their scripts start, again after a restart (the new VMs replace the old), and when a debugger is set while the game runs. `set_debugger(None)` detaches them, and each frame services them like the game context.

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

**Amendment (as built, WP26 PRs "http" #92, "templates" #104 and "eval" #124).**
- The small blocking MCP clients live in `re_mcp::client` (`HttpClient` over streamable HTTP, `StdioClient` for a `real mcp` child process), shared by the tests and the harness, still with no client crate.
- `real agent init --http ADDR` writes the Claude Code entry and prints how to start the server, plus the URL and the `Authorization` header for other clients; it does not print a snippet per client.
- The evaluation harness serves MCP by running `real mcp` on the copy as a child process, over the chosen transport (`--transport http|stdio`, HTTP by default), not in-process, so the server under test is the one people run.
- With every M3 command so far, `real mcp` lists 53 tools (WP24's structural override commands and WP21's four animation graph commands included).

## ADR-0047: Block-compressed textures with block_compression 0.10.1, encoded at import

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP20). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: WP20's phase-0 measurements (research bundle `m3/wp20/measurements.md` §2 and §4, and the encoder bench they summarise). Agreed with WP19 for ADR-0037 (see Consequences).

**Context.** Textures are imported by `re_assets` as RGBA8 (`re_types::render::TextureData`, `TextureFormat::{Rgba8Srgb, Rgba8Unorm}`), cached in `.real/cache` with postcard, and uploaded by `Renderer::upload_texture`, which builds the mip chain on the CPU. A 2048² texture is 16 MB in the cache and 21 MB on the GPU with mips. M3 asks for BC7 or BC1/BC3 on desktop through the asset cache, picked per adapter feature, with an uncompressed fallback, and for the encoder crate to be chosen after checking crates.io (M3_PLAN, WP20 PR "textures").

What was checked (measurements §4):
- wgpu exposes BC1–BC7 as one feature, `TEXTURE_COMPRESSION_BC`. lavapipe and llvmpipe GL both have it (wgpu-hal 30.0.1 `vulkan/adapter.rs:745-748`; on desktop GL it needs the S3TC, RGTC and BPTC extensions, `gles/adapter.rs:535-561`). BC7 blocks uploaded as `Bc7RgbaUnorm(Srgb)` and read back were bit-identical to the CPU decode on both. An adapter that has BC1/BC3 also has BC7, so BC1/BC3 buy nothing on desktop, and they scored 30.4 dB against BC7's 43 dB on the test image.
- Candidates, all from crates.io sources and metadata:

| Crate | Version | License | BC7 | Build | Result |
|---|---|---|---|---|---|
| block_compression | 0.10.1 (2026-10-11) | MIT | yes, plus BC1–BC5, BC6H; CPU encoder and decoder | pure Rust; with `default-features = false` its only dependency is bytemuck (ours) | 2048² BC7 `very_fast`: 15.9 s on 1 thread, 5.9 s on 4; 42.95 dB |
| intel_tex_2 | 0.5.0 | MIT/Apache-2.0 | yes | links 32 MB of prebuilt static libraries shipped in the crate for each target; they cannot be rebuilt from the published crate (its `ispc` feature needs the omitted `vendor/` sources and the ISPC compiler) | 4–6× faster than block_compression, 0.1–0.2 dB better |
| ctt-bc7f | 0.6.0 | (MIT OR Apache-2.0 OR Zlib) AND Apache-2.0 | yes | C++17 via cc | about 6 dB worse |
| dds | 0.2.0 | MIT OR Apache-2.0 | yes | pure Rust | pulls glam 0.25.0; fails our bans (ADR-0028) |
| texpresso | 2.0.2 | MIT | **no** (BC1–BC5) | pure Rust | — |
| ktx2 0.5.0, ddsfile 0.6.0 | | Apache-2.0, MIT | readers only | | not needed (below) |

`cargo deny check` with our `deny.toml` passes for block_compression with no exception; it builds on Rust 1.95.0; `cargo xtask dupes` stays clean.

**Decision.**
- **Encoder: `block_compression = { version = "=0.10.1", default-features = false, features = ["bc15", "bc7"] }`**, in `re_assets` only, behind a new `re_assets` feature `texture-encode` that `re_cli`'s `tooling` feature turns on. The runtime-only binary (`--no-default-features`) does not link it, and `cargo xtask check-graph` adds `block_compression` to its forbidden list for that build (ADR-0027). Its `wgpu` feature (a GPU encoder) stays off; it would resolve to our wgpu 30.0.1 if a later ADR wants it.
- **Decoder: `bcdec_rs = "=0.2.0"`** (MIT, safe Rust, no dependencies, decode only) in `re_render`, for the fallback below. The bench's GPU round trip was bit-identical to it on both backends. Tests cross-check it against block_compression's own decoder (a dev-dependency of `re_render`).
- **Formats.** `re_types::render::TextureFormat` gains `Bc7Srgb` (base colour and emissive), `Bc7Unorm` (metallic-roughness and other linear RGBA data) and `Bc5Unorm` (tangent-space normal maps; the forward shader reconstructs Z from X and Y for every normal map, compressed or not). `TextureData` gains `levels: Vec<Vec<u8>>` (mip levels 1..n, default empty): compressed textures carry their encoded mip chain, because the renderer cannot build mips from blocks. RGBA8 data is unchanged and still gets its mips at upload. These are maintainer-reviewed `re_types` changes with defaults.
- **Where encoding happens.** With `texture-encode` on, the texture importer in `re_assets` encodes each level (box-filtered as `mip_chain` does today, then encoded) into the cached blob. Levels smaller than 4×4 are padded by edge replication. A texture whose width or height is not a multiple of 4 stays RGBA8, because BC textures need block-aligned base sizes. Strips of block rows are encoded on the `re_core` pool; the output is identical for any thread count. Without the feature (a packaged game importing a texture stored as source), the importer keeps RGBA8, and `real build`'s report lists such textures as uncompressed. This is a maintainer-reviewed `re_assets` PR by WP20, as WP21's skin import is.
- **Settings.** `real.toml [assets] texture_compression = "bc"` (default) or `"none"`, and `texture_quality = "fast"` (default, the `very_fast` presets) or `"high"` (the `basic` presets, about 1 dB better and 3–4× slower). The encoder's crate version and the preset are folded into `TEXTURE_IMPORTER_VERSION`, and the format and preset into the settings bytes of the cache key, so the cache and the archive (`Processed { importer, version }`, refused on mismatch with `RE0602`, ADR-0037) both see a change. `re_assets` declares `pub const TEXTURE_REPEAT_AT_LOAD: bool = false` beside it, the static mark ADR-0037's `assets = "auto"` reads.
- **Upload.** `Gpu` requests `TEXTURE_COMPRESSION_BC` when the adapter has it. `upload_texture` uploads BC data as-is when the device has the feature; without it, it decodes every level to RGBA8 on the CPU with `bcdec_rs` (one-time, at upload) and logs the fallback once. `RE6015` covers malformed BC data (wrong level count or byte length).
- **No KTX2 or DDS reader in M3.** The cache stores our own postcard blob. Importing pre-compressed artist textures would use `ktx2` 0.5.0 or `ddsfile` 0.6.0 under a later ADR.
- ASTC and ETC2 are not used (desktop targets only, as the plan says).

**Alternatives.**
- **intel_tex_2.** The fastest and marginally best, but the shipped prebuilt archives cannot be rebuilt or audited from the published crate, which is against the spirit of DEPENDENCIES policy 1. It stays the documented alternative if import time becomes the problem; swapping is local to the importer.
- **BC1/BC3.** Half or the same size as BC7 but 12 dB worse here, and no adapter has them without BC7.
- **GPU encoding (block_compression's `wgpu` feature).** Faster on real GPUs, but import must work headless on any adapter and give the same bytes everywhere (the cache key); the CPU path does.
- **Encoding at load or in packaged games.** At 5.9 s per 2048² texture on 4 threads, a game with a hundred textures would take minutes on first boot.

**Consequences.**
- Cached and packaged textures shrink 4× against RGBA8 (BC7 is one byte per texel; mips add a third), and GPU memory shrinks the same.
- Import gets slower: about 6 s per 2048² texture on 4 cores at the default quality. The cache makes it a one-time cost per change.
- **ADR-0037 (packaging):** the texture importer marks itself too slow to repeat at load, so `assets = "auto"` always stores its output processed; the encoder never runs in a packaged game's load path. The runtime-only binary links only `bcdec_rs`. Agreed with WP19's thread; ADR-0037's decision needs no change. Measured on the five 2048² textures of Khronos' DamagedHelmet, the model ADR-0037 measured (measurements §5): source JPEGs 3.21 MB; RGBA8 with mips 111.8 MB, 23.1 MB under zstd 19 (7.2×); **BC7 `very_fast` with mips 27.96 MB, 12.48 MB under zstd 19 (3.9×)**, since zstd takes only 10–25% off BC7 blocks. So BC7 still exceeds `auto`'s 2× rule against the source, and the "slow importer, always processed" mark is what keeps it encoded; converting at first launch instead would cost about 17–18 s per 100 MB of texels at `very_fast` (the helmet's 3.2 MB of JPEG decode to 112 MB). Proposed addition to ADR-0037's Consequences, next to the 5× figure: "With ADR-0047's BC7 textures, the helmet's processed textures are 12.5 MB under zstd (3.9× the source JPEGs, against 7.2× for RGBA8); textures are always stored processed, because encoding them at first launch would cost about 18 s per 100 MB of texels."
- A cold cache now pays the encoder on first import (about 6 s per 2048² texture on 4 cores), including CI's `verify-demo` and `verify-plaza`; their projects' textures are small, and a later CI cache of `.real/cache` is the remedy if that changes.
- New `DEPENDENCIES.md` rows (the "Rendering" table): `block_compression` 0.10.1, MIT, MSRV 1.80, "BC7/BC5 encoder at import (`re_assets`, feature `texture-encode`; tooling only); dev-dependency of `re_render` for decoder cross-checks", features `bc15`, `bc7` (default features off), verified by the WP20 phase-0 bench (encode, decode, GPU round trip on lavapipe and llvmpipe GL, cargo-deny), Provisional until PR "textures" merges; `block_compression` is listed as tooling-only; and `bcdec_rs` 0.2.0, MIT, BC decoder for the no-BC fallback (runtime, `re_render`).

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
  - the type ids we decode are 0x01 to 0x0E, 0x10 and 0x12 to 0x22 (string, bool, int32, float32, float64, `UDim`, `UDim2`, `Ray`, `Faces`, `Axes`, `BrickColor`, `Color3`, `Vector2`, `Vector3`, `CFrame`, enum, referent, `Vector3int16`, `NumberSequence`, `ColorSequence`, `NumberRange`, `Rect`, `PhysicalProperties`, `Color3uint8`, int64, shared string, bytecode, optional `CFrame`, `UniqueId`, `Font`, and the newer content type). An unknown type id skips that property for that class with `RE9810`, never the file.
- **XML format facts:** one `roblox` element with `version="4"`; `Item` elements with `class` and a file-unique `referent` (the literal `null` is reserved for an empty reference); one `Properties` element per item whose children are typed elements carrying the property in a `name` attribute (`CoordinateFrame` with twelve children `X` to `R22`, `Ref`, `token` for enums, `BinaryString` and shared strings in base64, `ProtectedString` for script source, and so on); an optional `SharedStrings` table keyed by its `md5` attribute; `Meta` and the legacy `External` elements are read and ignored.
- **Serialised names.** Files store some properties under names scripts do not see (lower-case or legacy names, and colour stored as `Color3uint8`). `crates/re_import/data/serialized_names.json` maps them to our class properties, written by hand from the classes our data covers (ADR-0034). A property with no mapping is listed in the import report with `RE9811`, never dropped without a word.
- **What the reader does not import.** Terrain voxel data and CSG solid data are kept out of the scene and listed in the report (terrain voxels are an M3 non-goal). Asset ids stay ids and resolve through the substitute table (`RE9350` at use), as they do today.
- **Untrusted input.** Every length is checked against the bytes left before anything is allocated; a chunk may expand to at most 256 MB, a file to at most 10,000,000 instances; references to missing referents and parent cycles are errors with the chunk and offset (`RE9809`). The reader never panics on bad input: the tests feed it truncated and bit-flipped files built from our synthetic fixtures.
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
- `re_import` keeps `RE9800`–`RE9899`: `.rbxl`/`.rbxlx`/`.rbxm`/`.rbxmx` are read; `RE9801` keeps its meaning (unreadable input). New are `RE9809` malformed place file, `RE9810` unknown value type, `RE9811` unmapped serialised property (amended in PR "place files", #112: `RE9802`–`RE9804` were already `re_import`'s loader-syntax, unrecognised-statement and project-unsuitable codes).
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

**Amendment (M3, WP27 PR "incremental render"; accepted by the maintainer, 2026-10-15).** `re_compat::render::build` stops walking every part each frame. WP20 measured it at about 1.2–1.3 s per frame on ADR-0031's 250,000-part bigmap, against 38.6 ms for `Renderer::render` after culling (research `m3/wp20/measurements.md` §1), so the walk is the frame's bottleneck on large maps.
  - **Where the time goes.** On every frame the build lists every `BasePart` and `Light` by a dynamic query, clones the static-cell table, walks each part's ancestors to test "is it in `Workspace`", and reads each part's properties by name to rebuild its `FrameItem`. All of that is per part, even when nothing moved.
  - **A change journal is the feed, not a scan.** bevy_ecs change ticks (ADR-0005) tell us whether a component changed, but only when we ask each entity or iterate each matching archetype row, and `Changed<T>` needs a Rust type while the compat classes' `Compat_<Class>` components are host components registered at run time (ADR-0032). Asking each of 250,000 entities every frame is still a walk. So the build is fed by a **journal of what changed**, filled at the few places where changes happen:
    - **Field writes.** Every compat property write, tween, editor or agent command ends in `re_reflect::Registry::set_field` (or `insert`/`remove`). The registry already skips equal writes. When the world holds a `ChangeJournal` resource, each write that changes a value appends `(entity, component key)` to it. A world without the resource pays nothing. This is a small change in `re_reflect`, a maintainer crate: WP27 writes it as a maintainer-reviewed PR, or the maintainer makes it.
    - **Structure.** Spawning, despawning and reparenting go through `re_compat::instance::tree` and through `re_command` ops. Both append the entity, and on a reparent its subtree, to the journal. An observer on `ChildOf` catches the rest, so a path missed by the first two is still caught.
    - **Physics poses.** The compat physics write-back (`physics/pose.rs`, `physics/sim.rs`) writes `Transform` directly for bodies that moved, so it appends those entities itself. Sleeping and anchored bodies cost nothing.
  - **Retained state in the render hub.** Per part, the hub keeps its `FrameItem` (or its static cell and slot), whether it is in `Workspace`, and its looks key. Per cell, it keeps the merged meshes. It also keeps the lists of lights and effects. Each frame the build drains the journal, recomputes only the touched parts and the cells they belong to (a touched cell part still splits out as ADR-0031 says), and copies the retained items into the frame. Camera-dependent work (billboards, particles, beams, highlights) stays per frame, as it is bounded by those instances, not by parts.
  - **What "fixed cost" means.** On a frame where nothing changed, the build does no per-part work. The remaining cost is copying the retained draw items (about 23,000 on the bigmap, one per cell and material plus loose parts) and the camera-dependent work.
  - **Acceptance.**
    - **Idle frames visit no parts.** A counter test asserts that an idle frame visits zero parts on the 60,000-part and 250,000-part maps; it runs in `cargo test`.
    - **Bigmap timing.** Measured with WP20's harness, the build's mean on the idle 250,000-part map is at most 5 ms in the reference container (from about 1,300 ms), and a frame that moves 100 loose parts or touches one cell part is at most 5 ms more.
    - **Equivalence with a full rebuild.** A test runs random script edits (properties, reparenting, destroy, clone, tweens and physics moves) for many frames. On each frame it compares the incremental frame with a full rebuild from scratch, and they must be equal.
    - **Goldens.** The existing render goldens are unchanged, and `verify-plaza` stays green.
  - **Safety net.** In debug builds, `REAL_RENDER_VERIFY=1` makes every 60th frame also do a full rebuild and compare, and it fails loudly on a difference. A write path that skips the journal is a bug, and this check finds it in tests.
  - **Unaffected.** The build only reads the world, so simulation and D2 are untouched. After this merges, WP20 re-measures the render-thread threshold (ADR-0038).
  - **Order.** "incremental render" lands after "place files" and before "surfaces". The SurfaceGui and ViewportFrame redraw tracking then reuses the same journal.
  - **Accepted with conditions (maintainer).**
    - The `ChangeJournal` type lives in `re_scene`, next to `World`. `re_reflect` sees it only as an optional sink, through a small trait or a resource lookup, so `re_reflect` gets no compat concepts.
    - The hook does nothing unless the resource exists. The PR includes a bench showing that `set_field` costs the same without it.
    - Every write that bypasses `Registry` (native systems that mutate `Transform`, `re_anim` poses, and tweens or anything else that goes around `set_property`) either feeds the journal or is caught by `REAL_RENDER_VERIFY` and the randomized test. The PR description lists the ones found.
    - WP27 writes the hook in its "incremental render" PR, and the maintainer reviews that part.
  - Maintainer conditions: the `ChangeJournal` type lives in `re_scene` beside `World`, and `re_reflect` reaches it as an optional sink, so `re_reflect` learns no compat concepts; the PR shows `set_field` costs the same without the resource; and any write that bypasses the registry (native systems on `Transform`, `re_anim` poses) either feeds the journal or is caught by the randomized equivalence test and `REAL_RENDER_VERIFY`, listed in the PR description.

**Amendment (as built, WP27 PRs "place files" #112, "incremental render" #127 and "saved data" #131).**
- **Place files (#112).** `real import` reads `.rbxl`, `.rbxm`, `.rbxlx` and `.rbxmx` (`InputFormat::Place`), and `--parent` (`ImportOptions.parent`) puts the imported instances under a given instance. In the binary format, the reader takes acoustic absorption for `PhysicalProperties` when flag bit 1 (value 2) is set. That is an assumption from the format facts and has not been checked against a real file.
- **Incremental render (#127).** The `re_reflect` hook is an optional change sink on `Registry`, a function pointer: `set_field` calls it only when the value changed, and `insert` and `remove` call it too, so `re_reflect` sees no journal type. `ChangeJournal` holds at most `ChangeJournal::CAP` (65,536) entries between drains; past that it empties itself and the next build is a full pass. CI's `test` job runs with `REAL_RENDER_VERIFY=1`. Measured on the 250,000-part bigmap in the WP's 4-core container, not the reference container: idle frames 2.7 ms with no part visited, 100 falling parts 2.9 ms, one cell part recoloured every frame 5.2 ms, against about 2.4 s for the first, full build. All are inside the budgets above.
- **Saved data (#131).** Data is written through `ProjectFiles::writable(Data)`, not to `.real/data/` directly. `real run` still maps that to `.real/data/`; in the browser it is IndexedDB (ADR-0049 §6), so compat saves last across page loads.

**Amendment (WP27 PR "skins and effects", #153): §5 as built.**
- `BloomEffect`, `BlurEffect` and `SunRaysEffect` are `implemented`. Blur uses WP20's `blur_radius` and sun rays use its `sun_rays`. Several enabled effects combine: blooms add their intensities and take the widest size and the lowest threshold, blurs add as Gaussians do (radii in squares), and sun rays add their intensities and take the widest spread. `Size` 24 is a radius of 5% of the view's height, WP20's default bloom radius. Each kind of effect a place has replaces the project's `[render.post]` setting of that kind. `DepthOfFieldEffect` stays `partial`, since WP20 has no depth of field.
- `MeshPart` and `Bone` exist. A `MeshPart` whose `MeshId` resolves through `[compat.assets]` to a glTF file draws that model, scaled to its `Size`; an unknown id draws a block, and a file that does not load draws a block and warns once (`RE9350`). `MeshPart.TextureID` stays `partial`: the part's `Color` and `Material` are used.
- A skinned model is a skinned draw item on WP21's skins. Each `Bone` under the part drives the joint with the same name; its `Transform` applies on top of the joint's rest pose, in the model's units, before the part's scale. The Animator poses `Bone`s by name, as it poses `Motor6D`s (a `Motor6D` wins a shared name). A bone or joint without a partner is `RE9440`, once per part.

## ADR-0049: Browser builds: `wasm32-unknown-unknown` with wasi-sdk for Luau and Jolt, WebGPU first, the archive in memory, one thread

**Status:** Accepted (maintainer, 2026-10-20). §2 approved by the owner on 2026-10-20 ("Copies plus upstream"): the patched copies go in `third_party/` until releases carry the changes, and the owner submits the patches upstream. §11 is a recommendation to ADR-0036, which WP18 decides in PR "d2". Evidence: a spike on 2026-10-15 in the reference container that ran Luau, Jolt, `re_render`, egui and `re_audio` in one wasm module in headless Chromium 141, plus type-checking all of `re_app` for wasm. Numbers are in `research/m3/wp29/measurements.md`; sources, the build environment and every patch are in `research/m3/wp29/spike/`. crates.io was checked the same day: mlua 0.12.2 and oxijolt 1.1.0 are the newest releases.

**Context.** The owner chose to add browser builds to M3 ("Both", 2026-10-15). This reverses the M3 plan's default "The web (wasm with WebGPU) and consoles are not in M3" and WP19's non-goal "the web". A game made with Real Engine must also run in a browser, from a folder a static web server can host.

The runtime has four parts that do not port for free:
- **Luau** (mlua's vendored C++) and **Jolt** (oxijolt-sys, C++ built by CMake) need a C and C++ toolchain and C library for wasm. The crates.io-only rule (DEPENDENCIES.md: no git dependencies, no path dependencies outside the workspace, no build scripts that download things) still applies.
- **wgpu 30** has a WebGPU backend and a WebGL2 backend, and winit 0.30 has a web event loop, but the engine's GPU setup blocks on futures (`pollster::block_on`), which deadlocks a browser's only thread.
- **Files.** ADR-0037 puts every runtime read behind `ProjectFiles` with synchronous `FileLayer::read`; a browser has no file system and fetches asynchronously.
- **Threads, clocks, audio.** `re_core::TaskPool` is a rayon pool; `std::time::Instant` panics on `wasm32-unknown-unknown`; browsers start audio only after a user gesture.

ADR-0010 (the frame loop is ours) and ADR-0036 (D2 determinism, which asked the web WP to measure glam) constrain the answers.

**Decision.**

**1. Target: `wasm32-unknown-unknown`, with wasi-sdk compiling the C and C++.**
- **Rust target.** `wasm32-unknown-unknown`, with wasm-bindgen for the JavaScript glue. wgpu's WebGPU backend is behind `cfg(webgpu)`, which wgpu sets only when the target is not Emscripten, and winit's web platform needs `target_os = "unknown"`; so Emscripten (which mlua supports officially) cannot host the renderer.
- **C and C++.** wasi-sdk 34's clang with `--target=wasm32-wasip1 --sysroot=<wasi-sysroot>` compiles Luau and Jolt for that target, and the link takes wasi-libc and wasi-sdk's exception-enabled libc++, libc++abi and libunwind (`lib/wasm32-wasip1/eh`). The `cc` crate reads `CXX_wasm32_unknown_unknown` and `CXXSTDLIB_wasm32_unknown_unknown=c++`; mlua reads `LUAU_CXXFLAGS`; the link flags go in `.cargo/config.toml` under `[target.wasm32-unknown-unknown]`.
- **Exceptions.** Luau throws C++ exceptions, so it is built with `-fwasm-exceptions -mllvm -wasm-use-legacy-eh=false` (the new exnref encoding). Chromium refuses a module that mixes the legacy and new encodings, and Rust code itself is built with `panic = "abort"` on wasm, so it adds neither.
- **WASI imports.** The C side imports 12 WASI preview1 functions (clocks, `fd_write` for stdout and stderr, and stubs). A 60-line JavaScript shim provides them; stdout and stderr go to the browser console. No file system is emulated.
- **Static constructors.** The web entry point calls wasm-ld's `__wasm_call_ctors` once at start. Without an explicit reference, wasm-ld runs the constructors before every export call, and Luau's flag registry loops.
- **Why not the alternatives.** Emscripten is excluded above. A C++ toolchain other than wasi-sdk would need its own C library for `wasm32-unknown-unknown`; wasi-sdk ships one with exceptions, is a single archive with a published sha256, and its CMake toolchain file builds Jolt unchanged.
- **The toolchain is a tool, not a dependency.** Like CMake for Jolt on desktop, wasi-sdk is installed on the machine that builds the web runtime. CI installs a pinned release checked by sha256 (`tools/ci/wasi-sdk.sh`). Nothing downloads it during a cargo build. A game developer who uses a prebuilt web runtime (§7) never needs it.

**2. Two small changes in mlua and oxijolt-sys, upstream first.**
- **mlua 0.12.2** calls `libc::tmpfile` and four stdio functions in `luau/heap_dump.rs`, which the `libc` crate does not define for `wasm32-unknown-unknown`. The change gates them so `heap_dump` returns `None` there (19 lines).
- **oxijolt-sys 1.1.0** refuses 32-bit targets. The change registers a `wasm32` target family, commits bindings for it (bindgen must run with `-fvisibility=default`, because wasm's default hidden visibility makes it emit no functions), and passes `-DJPH_PLATFORM_WASM` and wasi-sdk's CMake toolchain file to Jolt's build (Jolt's `Core.h` recognises wasm only under `__EMSCRIPTEN__`). 36 lines plus generated bindings.
- **Upstream first.** Both changes are submitted upstream, which is an outward-facing step the owner approves. When releases carrying them reach crates.io, the workspace bumps to them and nothing else changes.
- **Until then: in-workspace copies.** If no release has them when WP29 PR "runtime" is ready, the two crates are copied, with their licence files, to `third_party/mlua/` and `third_party/oxijolt-sys/` and used through `[patch.crates-io]`. DEPENDENCIES.md allows path dependencies inside the workspace, but a patched copy of two core crates is a dependency change, so it needs the maintainer's DEPENDENCIES.md rows and the owner's approval. Both changes are `cfg`-gated to wasm32 and change no native code; `cargo xtask d2`'s native hashes prove it. The copies go when the releases arrive.

**3. Async only at the edges: GPU setup and captures.**
- `re_render::Gpu` gains `headless_async`, `for_surface_async` and `from_instance_async`. The blocking constructors become `pollster::block_on` of these, so desktop behaviour is unchanged (spike diff: 60 lines in `gpu.rs`).
- `Capture` gains an async readback (`map_async` then a later frame). On WebGPU, dropping a buffer destroys it, so the capture keeps its staging buffer alive until the map resolves. The blocking `read_back` stays for desktop.
- `re_render`'s limit picker falls back to `wgpu::Limits::downlevel_webgl2_defaults()` when the adapter is WebGL2.
- Nothing else becomes async. The frame body of ADR-0010 is unchanged and synchronous.

**4. The frame loop: winit's web event loop drives ADR-0010's frame.**
- `re_app` runs the same `App` frame. On the web, `run_windowed` uses `EventLoop::spawn_app` (winit's non-blocking entry point) instead of `run_app`, and redraws on `requestAnimationFrame` through winit's `RedrawRequested`.
- Because GPU setup is async (§3), the web window path has one more state: the window exists and the GPU is pending. Frames are not simulated until the GPU is ready, so the first simulated frame is the same as on desktop.
- The game's canvas is the page's `<canvas id="real">`, sized by CSS and the device pixel ratio. Fullscreen and pointer lock need a user gesture and go through winit's web extensions.
- The headless runner (CI, §8) drives frames from a JavaScript loop with no canvas and renders into a `Capture`, as `real run --headless` does.

**5. Clocks and threads.**
- **Clocks.** `std::time::Instant` panics on `wasm32-unknown-unknown`. `re_core::time` re-exports `web_time::{Instant, SystemTime}` (web-time 1.1.0, already in the lockfile through winit and egui; on desktop it re-exports `std::time`). A wasm-only clippy pass in the `build-web` CI job (`CLIPPY_CONF_DIR=tools/ci/clippy-web`) bans `std::time::Instant::now` and `SystemTime::now` with `disallowed-methods` and the reason "panics on wasm32-unknown-unknown; use re_core::time". *Amended 2026-10-20 (WP29 PR "toolchain"):* the ban does not go in the root `clippy.toml`, because on desktop web-time re-exports `std::time`, so a desktop clippy cannot tell `re_core::time::Instant::now` from the banned path. The web list repeats the root file's libm list (ADR-0036), which `cargo xtask clippy-web` checks. *Amended in PR "runtime" (#119):* it also bans `std::process::id`, which panics on wasm, in favour of `re_core::process_id`. Today's users: `re_script` (budget, scheduler), `re_proc` (evaluation timing), `re_app` (5 files), `re_scene` (`ids.rs`).
- **One thread.** Wasm threads need nightly `build-std` with atomics and a cross-origin-isolated page (COOP and COEP headers), which many static hosts cannot send. The web build is single-threaded in M3. `TaskPool::new` on wasm creates a pool that runs every task on the calling thread at the next join or scope. The spike checked rayon with `use_current_thread()` and one thread: `scope` and `par_iter` work, a detached `spawn` never runs, so `TaskPool::spawn` on wasm runs the task before it returns (as built in PR "portable", #89; the draft said the frame's task barrier would run it, and running it at once is simpler with the same result for the frame). Jolt's job system already runs its jobs at its barrier (measured).
- **Cost.** One thread is what a phone-class web game gets anyway. Jolt runs about 1.6 times slower in wasm than natively (329–371 ms against 217 ms for 300 steps of a 1,000-box pile, deterministic build), and Luau about 1.3 to 1.8 times (`measurements.md` §5). The spike's 64-body scene ran at 0.2–0.3 ms of physics per frame.

**6. Files: the archive in memory, behind ADR-0037's layers.**
- **Read side.** At boot, the page fetches `game.rpak` whole, showing progress, and hands the bytes to the runtime, which copies them once into wasm memory and opens them with WP19's archive reader over a byte slice (a `FileLayer`). `FileLayer::read` stays synchronous, and every crate reads through `ProjectFiles` exactly as in a packaged desktop game. This works on any static host: no `Range` support and no CORS headers are needed when the archive is on the same origin.
- **Size.** Wasm32 memory is at most 4 GiB and a browser tab may get less. `real build --target web` warns above 256 MiB of archive (`RE0974`), and the runtime reports a fetch or allocation failure as `RE0622`. Fetching entries on demand with `Range` requests (WP19's notes: header, index, then entries) is the later step for large games; it needs an async read or a prefetch set, which this ADR does not settle.
- **Writable folders (ADR-0037 left `writable()` open for this ADR).** The browser has no per-user folders. `ProjectFiles::writable(kind)` returns a `WritableDir` handle instead of a `Path`, with `read`, `write`, `remove` and `list` on project-style relative paths:
  - desktop: a thin wrapper over the same directories as today (`Path` stays reachable through `WritableDir::path()` for tools, returning `None` on the web);
  - web, `Data`: an in-memory map loaded from IndexedDB before the first frame and written back after each write (write-behind; a write that IndexedDB refuses is `RE0624`, a warning, and the game keeps running with what it has in memory). Keys are prefixed by the package `identifier`;
  - web, `Cache`: memory only;
  - web, `Crash`: the report is printed to the console;
  - web, `Out`: memory, offered as a download when a runtime command writes a capture.
  IndexedDB is reached through web-sys features, so no crate is added. WP27's persistence (ADR-0048) already writes only through `writable(Data)`.

**7. `real build --target web`.**
- `real build --project P --out DIR --target web` writes a folder a static server can host:
  - `index.html` (the canvas, a progress bar, the click-to-start overlay of §9, and the boot script),
  - `real.js` (wasm-bindgen's glue) and `wasi.js` (§1),
  - `real_bg.wasm` (the web runtime, optimised with `wasm-opt -O2` and stripped when binaryen is present; without it the build warns `RE0973` and copies the unoptimised module),
  - `game.rpak` (ADR-0037's archive, unchanged) and `THIRD-PARTY-NOTICES.txt` (WP19's notices, from the web runtime's dependency graph).
- **The web runtime** is the crate `re_web` (a `cdylib` with wasm-bindgen exports over `re_app`'s runtime features, ADR-0027). As on desktop (ADR-0037 step 3), `real build` builds it with cargo for `wasm32-unknown-unknown` when the toolchain is installed, or takes `--runtime DIR` holding a prebuilt `real_bg.wasm` and `real.js`. Release CI publishes the prebuilt web runtime next to the desktop runtimes, so most developers need neither Rust's wasm target nor wasi-sdk. A missing toolchain is `RE0970`, with the install steps in its explain page; a failed runtime build is `RE0971`; a wasm-bindgen CLI whose version differs from the lockfile's is `RE0972`.
- **Hosting.** Any static server works. The page needs a secure context for WebGPU (HTTPS, or `localhost`); `application/wasm` as the module's MIME type enables streaming compilation, and the boot script falls back to compiling from an `ArrayBuffer` when a server sends another type. No COOP or COEP headers are needed (§5). Compression is left to the host.
- **What a web build refuses.** A project whose `[package]` asks for something the web runtime lacks fails the build with `RE0975`: a dedicated-server role (§10), or `[package] web = false`.

**8. Rendering backends and CI.**
- **WebGPU first, WebGL2 second.** The runtime asks for a WebGPU adapter and falls back to wgpu's WebGL2 backend on an `OffscreenCanvas`-capable or plain canvas surface. With neither, the page shows `RE0620` (no WebGPU or WebGL2 adapter). The spike drew its scene on both (120 frames each). WebGL2 lacks compute, so WP20's compute passes need a fallback or are skipped on it; WP20's ADR-0038 decides which.
- **Browser support** needs WebGPU or WebGL2, and wasm exnref exception handling. Only Chromium 141 was verified. The boot script checks exnref with a 30-byte probe module and shows `RE0621` with the browser requirements when it is missing, before fetching the archive. Firefox and Safari are reported, not required, in M3.
- **CI.** A `web` job (through WP18 and the maintainer) installs wasi-sdk and Chromium (Playwright, pinned in `tools/web-test/package-lock.json`), builds the web runtime and the island and plaza with `real build --target web`, serves each folder with a std-only static server in xtask, and runs them in headless Chromium with WebGPU enabled (`--enable-unsafe-webgpu`, SwiftShader on the runner's CPU).
- **Headless, not windowed.** In this headless Chromium, configuring any canvas makes every later `mapAsync` fail ("A valid external Instance reference no longer exists"); it reproduces in plain JavaScript with no engine code. So CI runs the runtime's headless mode (no canvas, captures read back in wasm), which works, and the windowed path is checked by booting it and counting presented frames without reading pixels. If a runner's Chromium has no WebGPU at all, the job runs the same checks on WebGL2 and says so in its log; that is the documented fallback.

**9. Audio and input.**
- **Audio.** kira 0.12.5 with cpal's WebAudio backend works unchanged (spike: AudioContext running at 48,000 Hz, one sound playing). Browsers start an AudioContext only after a user gesture, so `index.html` shows a click-to-start overlay and creates the audio device on that click; until then `re_audio` runs its headless backend, so the frame and `state_hash` do not depend on when the player clicks. CI starts Chromium with `--autoplay-policy=no-user-gesture-required`.
- **Input.** Keyboard and mouse come from winit's web events, pointer lock from winit's web extension. Gamepads go through gilrs 0.11's web backend (the browser Gamepad API), which the spike did not exercise; PR "runtime" tests it with a scripted gamepad in Chromium.

**10. Online play in the browser (with ADR-0050).**
- ADR-0050's transport seam takes a WebSocket implementation for the browser client, through web-sys's `WebSocket` (no new crate on the client). The native server needs a WebSocket listener beside UDP, which is ADR-0050's conditional PR "web" and its own dependency choice.
- **This ADR recommends that PR "web" is in M3**, after WP29 PR "runtime": the owner asked for both, and a Roblox-style game in a browser that cannot join its server is half a feature. A browser cannot be a server, so a web build is always a client (`RE0975` for a server role).

**11. Determinism: what the spike measured for ADR-0036.**
- Jolt's cross-platform-deterministic build gives the same 1,000-box pile hash in wasm as on native x86_64 (`a81d3cf9ef4ee502`).
- glam's `Vec3` and `Mat3` give the same bits. `Mat4`, `Quat`, `Vec4` and `Affine3A` do not: x86_64 uses SSE2 with a different operation order, wasm uses scalar code. Native x86_64 with glam's `scalar-math` feature matches wasm exactly.
- `scalar-math` costs little on x86_64 except for `Affine3A` products (8.24 against 3.57 ns) and `Quat` products (7.63 against 5.99 ns); `Mat4` products and `transform_point3` are unchanged (`measurements.md` §7). It also changes the alignment of `Vec4` and `Mat4`, which the render crates upload with bytemuck.
- **Recommendation to ADR-0036:** turn on glam's `scalar-math` on every target, which also removes glam's NEON fused paths on macOS arm64, and let WP20 check its uploads. Until ADR-0036 is amended, the web is a reported D2 platform, like macOS arm64; once it is, the web's `d2` line is required.

**12. Codes and owned paths.**
- `re_app` takes `RE0620`–`RE0649` for the web runtime: `RE0620` no WebGPU or WebGL2 adapter, `RE0621` the browser lacks wasm exception handling (reported by the page), `RE0622` the archive could not be fetched or held, `RE0623` the panic hook's report of a runtime panic, `RE0624` browser storage refused a write (warning), `RE0625` a feature the web runtime does not have was used (warning, once per feature). The rest are reserved.
- `re_cli` takes `RE0970`–`RE0989` for `real build --target web`: `RE0970` wasm toolchain missing, `RE0971` web runtime build failed, `RE0972` wasm-bindgen CLI version mismatch, `RE0973` binaryen missing (warning), `RE0974` archive over 256 MiB (warning), `RE0975` the project asks for something the web cannot do.
- `re_web` reports through `re_app`'s codes and needs no range.
- WP29 owns `crates/re_web/` (new), `crates/re_app/src/web/` (new), `crates/re_cli/src/build/web.rs` (inside WP19's `build/`, reviewed by WP19), `tools/web-test/` (new) and `tools/ci/wasi-sdk.sh` (new). Everything else is a small PR in its owner's paths.

**Alternatives considered.**
- **Emscripten for everything.** mlua supports it and Jolt documents it, but wgpu's WebGPU backend is compiled out for it and winit has no Emscripten platform, so the renderer would need WebGL through a different path. Rejected.
- **Drop Luau or Jolt in the browser** (an interpreter in Rust, or a Rust physics engine). Scripts and physics would behave differently from desktop, which breaks D2 and the compat layer. Rejected; the spike shows both run.
- **Threads with SharedArrayBuffer.** Needs nightly Rust and COOP/COEP headers on every host. Deferred until wasm threads work on stable Rust.
- **Fetch entries on demand (Range requests) from the start.** Needs either an async `FileLayer` or a prefetch set per scene, and a host with `Range` support. Deferred (§6); the archive format already allows it.
- **Run the editor in the browser.** Out of scope: the editor uses the file system, `notify` and tooling crates that ADR-0027 keeps out of the runtime.

**Consequences.**
- Every crate that times something uses `re_core::time`, and clippy enforces it.
- `re_render` keeps two constructor shapes (async, and blocking wrappers) and two readback shapes.
- `ProjectFiles::writable` changes from `&Path` to `WritableDir` before ADR-0037's "files" contract freezes that part (WP19 and the maintainer decide; this is the one change to their API).
- mlua's `Integer` is `i32` on wasm32. `re_script` and `re_compat` convert with `i64::from` (9 sites in the spike). `re_script` normalises integral numbers itself so `state_hash` sees the same value on every platform.
- `uuid` gains its `js` feature (randomness from the browser).
- CI gains wasi-sdk, wasm-bindgen-cli 0.2.129 (matching the lockfile), binaryen and a pinned Playwright Chromium. The lockfile gains no crate: wasm-bindgen, wasm-bindgen-futures, js-sys, web-sys and web-time are already in it through winit, wgpu and egui; `re_web` takes them as direct dependencies (DEPENDENCIES.md rows through the maintainer).
- The spike's runner module was 3.96 MB gzipped without `re_app` and `re_compat`. PR "runtime" records the full runtime's size; the plan's budget is 8 MB gzipped for the web runtime, checked in CI as a reported number in M3.

**Amendment (as built, WP29 PRs "runtime" #119, "build" #132, and WP27 "saved data" #131).**
- **§5.** See the clock amendments in §5: the bans live in the wasm-only clippy pass, which covers every runtime crate (19 crates with `re_net` and `re_web`).
- **§6.** The archive's in-memory source is `Archive::from_bytes(name, bytes)`: the same reader, hash and version checks as a file, behind one `read_at`. The archive module and zstd build for wasm32 (zstd-sys with wasi-sdk's clang), so zstd is in the web runtime. `Data` and `Crash` are memory-backed folders whose changes go to the page, which stores them in an IndexedDB database named `real-engine:<id>` under the keys `data/<path>` and `crash/<path>`; `<id>` is the archive's `[package] identifier` (or the project's name). Compat saved data writes through `writable(Data)` (#131), so browser saves last across page loads. When the browser refuses storage, saves stay in memory with a console warning.
- **§7.** The web runtime is built with `[profile.web]` (fat LTO, `codegen-units = 1`, `opt-level = "s"`), then `wasm-opt -Oz` with the runtime's wasm features named explicitly, instead of `-O2`. The output folder is `index.html`, `boot.js`, `wasi.js`, `real.js`, `real_bg.wasm`, `runtime.json` and `game.rpak`. `runtime.json` names the engine, Luau, bytecode and compiler versions and the target; `--runtime DIR` checks it, and an earlier web build is itself a valid runtime folder. CI fails a `real_bg.wasm` above 8 MB gzipped: 6.88 MB at PR "runtime", 7,223,011 bytes at PR "build". The notices file arrives with WP19 PR "bundles". `RE0975` is not defined yet: nothing triggers it until WP30's server role.
- **§8.** The WebGL2 fallback is chosen with `wgpu::util::is_browser_webgpu_supported` before the surface exists. The static server for the browser checks stays in Node (`tools/web-test/serve.mjs`), since Playwright needs Node anyway; `cargo xtask web-test` is the entry point CI runs. Since ADR-0036's amendment, the browser's replay hashes must match native.
- **§12.** The codes as built: `RE0620` no WebGPU adapter, `RE0621` the canvas could not be added or its surface failed while presenting, `RE0622` a Rust panic in the browser (reported to the console and the page). A missing exnref is caught by the boot script before the engine loads, without a code. `RE0623`–`RE0649` stay free for WP29. `re_cli`'s `RE0970`–`RE0974` are as listed.

**Amendment (WP19 PR "bundles", #161).** The web folder's notices are `THIRD_PARTY_NOTICES.txt`, the desktop name (§7 said `THIRD-PARTY-NOTICES.txt`), listed from `re_web`'s crates by ADR-0037's generator. The file is one of the web runtime's files (`RUNTIME_FILES`), so a prebuilt web runtime carries it, and a build with `--runtime DIR` copies it with the game's own notices added. A folder without it is not a web runtime (`RE0971`). `real build --runtime-only --target web` writes a web runtime folder, and the release workflow publishes that folder.

## ADR-0050: Online play: our own reliable UDP in `re_net`, client-owned characters, and a lockstep check over loopback

**Status:** Accepted (maintainer, 2026-10-15; drafted by WP30). The code waits for M2's definition of done (M3_PLAN, "How M3 starts"). Evidence: a two-process spike of the transport below over loopback with seeded loss and delay, a probe of quinn's dependency graph, and a measurement of the plaza's replication traffic, all on 2026-10-15 in the reference container (`research/m3/wp30/measurements.md`, `research/m3/wp30/spike/`). quinn 0.11.12 and quinn-proto 0.11.19 were checked on crates.io the same day.

**Context.** The owner chose to add online multiplayer to M3, so a Roblox-style game must be playable by people on different machines. This reverses the M3 plan's default "Real networking for Roblox-style projects: not in M3" and WP27's non-goal "real networking".

M2's WP12 already splits a compat game into an authoritative server world and the local player's client world (`re_compat::net`, `CompatWorlds`). Each frame, `replicate` copies the replicated containers from server to client and copies the components the local player owns back up, and `pump` delivers the messages (`Msg`: remote events and invocations, replies, prompt actions, chat, kicks) that queue in the `NetLink` both worlds share. The client already runs its own character's physics, because LocalScripts move the character on the client, as on Roblox. Everything crosses as Rust values inside one process:
- `replicate(&mut server, &mut client, &set)` writes straight into the client world, and its mirror of what was sent (`Replica`) is a resource of the client world, so there can be only one client;
- `Msg` and `NetValue` hold `Entity` handles and `re_reflect::Value`s, and neither has a serialised form (`re_reflect::Value` has no serde derive);
- `Shared` has one `local_player` and counts messages to other players as `undelivered`; players beyond the first (`[compat] players = N`) are simulated on the server only.

WP30 has to put a real transport under that seam, so the same game runs as one server process and any number of client processes, on one machine or across a network, without changing the scripts. The runtime constraint is ADR-0027: the runtime-only build (`re_cli --no-default-features`, which is also what WP19 packages) must not reach tokio, and `cargo xtask check-graph` enforces it.

**Decision.**

**1. Transport: our own reliability over `std::net::UdpSocket`, in a new crate `re_net`.**
- **The crate.** `re_net` is a subsystem crate in the ARCHITECTURE §2 sense. It depends on `re_core` (diagnostics), `serde` and `postcard`, all already in the workspace, and on nothing else: no async runtime, no threads, no new crate. It knows nothing about scenes or scripts. `re_compat` depends on it; `re_app` and `re_cli` reach it through `re_compat` and the run options (§6). It owns the codes `RE8900`–`RE8999`.
- **Polled from the frame.** A non-blocking socket is read at the start of a frame (`Endpoint::poll`) and written at the end (`Endpoint::flush`). The frame loop is the only clock, so a headless test drives both ends with no sleeps and no timing races.
- **Packets.** Every datagram is at most **1,200 bytes** (below the IPv6 minimum MTU less headers, so no IP fragmentation). Every packet starts with a 15-byte header: magic `RENT`, the protocol version (`u16`), the packet kind and the connection's salt (`u64`). A data packet adds its sequence number, the latest sequence received and a 32-bit field of the ones before it (`u16`, `u16`, `u32`), then chunks. A chunk names its channel, message id, fragment index and count, and length. All integers are little-endian.
- **Channels.** Two: **unreliable** (one datagram, dropped when lost) and **reliable ordered** (split into 1,170-byte fragments, each resent after the retransmission timeout until a packet that carried it is acked, and delivered in message-id order). A reliable message is at most 1 MiB. The sender keeps at most 1,024 messages in flight past the oldest unacked one. The retransmission timeout follows RFC 6298's smoothed RTT and variance, between two frame times and 1 s.
- **Handshake.** The client sends **Hello** (its salt, padded to 1,200 bytes). The server answers **Challenge** with a cookie, a keyed SipHash of the client's address and salt, which is smaller than the Hello and costs the server no state. The client answers **Response** (the cookie, padded), and the server creates the connection and answers **Accept**, or **Deny** with a reason: protocol version mismatch (`RE8903`), server full (`RE8904`), or a different game (`RE8905`, §5). A spoofed source address therefore gets no amplification and no allocation. Handshake packets are resent every 250 ms for up to 10 s (`RE8902`).
- **Liveness.** A connection that sends nothing for 100 ms sends an empty data packet (which also carries acks). One that receives nothing for **10 s** is closed (`RE8906`). A **Disconnect** packet, sent three times, closes it at once.
- **Bandwidth.** Each connection has a token bucket. The defaults are **256 KiB/s from the server to each client** and **64 KiB/s from a client** (`[net]`, §6). The reliable backlog a sender may have unacked is the bandwidth-delay product, `max(64 KiB, 2 × rate × smoothed RTT)`; the replication pass (§3) reads `Connection::backlog` and skips a connection that is over it, so changes coalesce instead of queueing.
- **A transport seam.** `Endpoint` sends and receives datagrams through a small trait with three implementations: UDP, an in-memory pair (tests, and the in-process two-world run of §3), and a link conditioner around either that drops, delays, duplicates and reorders packets from a seed. A browser client cannot open a UDP socket, so browser play needs a WebSocket implementation of the same trait and a WebSocket listener on the native server; that is a conditional PR "web" (§8), decided with WP29's ADR-0049.
- **Platforms.** `std::net::UdpSocket` is the same on Windows, macOS and Linux. On Windows, `recv_from` reports an earlier send's ICMP "port unreachable" as `ConnectionReset`; `poll` ignores that error and lets the timeout decide.
- **No encryption in M3.** Traffic is plain and unauthenticated beyond the cookie and salt. ADR-0050 targets games played on a LAN, across the internet between people who trust the host, and CI. The documentation says so, and `real run --server` prints it once at start.

*Why not QUIC through quinn.* The spike's numbers show our protocol does what M3 needs (`measurements.md` §1): over two processes on loopback, 3,000 reliable messages and a 2 MiB snapshot arrived complete and in order at 0, 5 and 20 % loss per direction and at 200 ms round trip, with the 2 MiB snapshot in 2.1 to 3.2 s at a 1 MB/s budget. Against that:
- quinn needs an async runtime (tokio, smol or async-std). A dedicated server is the runtime-only build, which ADR-0027 keeps free of tokio, and a second runtime for one feature is not worth its weight.
- quinn-proto, quinn's sans-IO core, avoids the runtime but adds 14 crates to the lockfile, including `ring` (C and assembly per target), and QUIC requires TLS 1.3: every server, including one a player starts on their own machine, would need a certificate that clients trust, which means self-signed certificates and trust on first use.
- Neither helps a browser, which cannot use raw QUIC either (only WebTransport, which needs HTTP/3 on the server).
- QUIC's real advantages, encryption and proper congestion control, are worth having later. The transport seam keeps the door open: a later ADR can add a quinn-proto implementation of the trait without touching `re_compat`.

**2. Authority and trust: the server is authoritative, except for what the Roblox model gives a client.**
- **The server decides.** Physics, server scripts, data services (WP27's persistence runs only on the server) and everything in the replicated containers are the server's. A client's world is a replica, as in M2.
- **A client sends four things:** its input-driven state for the instances it **owns**, remote events and invocations, proximity-prompt actions, and chat. Nothing else it changes reaches the server.
- **Why ownership and not input only.** A client must keep simulating its own character. On Roblox, LocalScripts move the character on the client (they set velocities, `CFrame`s and Humanoid state, and play animations), and the server never runs LocalScripts, so a server cannot recompute that from raw input. M2 already works this way (`NetworkOwner`, `ReplicationSet::upstream`, and the client's physics phase), and existing compat games depend on it. Sending only input would need client-side prediction and server re-simulation of script-driven movement, which the compat layer cannot do.
- **What an owner may change.** In a networked run, `ReplicationSet::upstream` defaults to the motion components: a part's transform and linear and angular velocity, the Humanoid's movement state (`MoveDirection`, `Jump`, `Sit`, `PlatformStand`, the state machine's state) and the `Animator`'s playing tracks. It is no longer every component (`"*"`, M2's default). A client's write to another component of an owned instance stays on that client and is reported once per component as `RE9451` (a warning in the client's output). The in-process two-world run keeps `"*"`, so M2's tests and the island and plaza replays do not change. A project can set `[compat.net] upstream` to widen or narrow the list.
- **Ownership** is what M2 has: `LoadCharacter` gives a player its character, and server scripts change it with `SetNetworkOwner`. A player's connection closing gives everything it owned back to the server.
- **Validation on the server.** An owned part that moves faster than `[compat.net] max_owned_speed` (default 1,000 studs per second, well above the vendor's terminal velocities) is put back where the server last had it, and the client gets the correction (`RE9452`, a warning on the server). A remote message larger than 64 KiB, or one that does not decode, is dropped (`RE9450`). A player who fires more than `[compat.net] remote_rate` messages a second (default 600, all remotes together) has the excess dropped, reported once per second (`RE9453`). None of these disconnects the player; a game's own scripts can `Kick`.

**3. Wire format: postcard over serde, one ordered frame message per tick and direction.**
- **Encoding.** postcard 1.1.3 (already a workspace dependency of `re_assets` and `re_proc`) with serde derives. The protocol version in every packet header is a single `u16`, bumped on any change to the wire types. Client and server must have the same version; there is no compatibility across versions, because both ends are the same engine build (`RE8903` otherwise).
- **Values.** `re_compat::net` adds serialisable forms of its messages: `WireValue` mirrors `re_reflect::Value` (with entity references as connection handles), and `NetValue`'s instances travel as handles. `re_reflect` does not change. Floats travel as stored, as f64, bit for bit, because the lockstep check (§7) compares hashes; quantisation is a later optimisation (the plaza's steady state would roughly halve, `measurements.md` §3).
- **Handles.** Each connection numbers the instances it has sent with `u32` handles. An instance's stable id goes once, in its create operation; references after that are handles of a few bytes, not 36-character ids. Component types travel as `u16` ids from a table the server sends in **Welcome** (§5), so names cross once per connection.
- **One message per tick.** Each direction sends, per tick, one reliable message `Frame { tick, ops, msgs }`. On the server, `ops` are the replication operations for that client (create, set field, merge map keys, set tags, rename, reparent, remove) and `msgs` are the remote events, replies, chat and kicks for that player, in the order they happened. On the client, `ops` are its owned instances' changes and `msgs` its remotes, prompts and chat. A single ordered stream keeps the order scripts rely on: a property set before `FireClient` is visible when the event arrives. `UnreliableRemoteEvent` uses the unreliable channel, one datagram per event; an event too large for one datagram is dropped with a warning on the sender (`RE9454`), as the vendor drops oversized unreliable payloads (the exact vendor limit is checked against its reference page in PR "wire").
- **The pass is split.** `replicate` becomes two halves: **diff** on the server, per connection, against that connection's mirror (the `Replica` moves from the client world to the server's connection state), producing ops; and **apply** on the client, by handle. The client's owned changes go the other way through the same two halves. The in-process two-world run uses the same diff, wire encoding and apply through the in-memory transport, so every M2 compat test exercises the network code, and several clients can share one server process in tests.

**4. Players, budgets and backpressure.**
- `Shared` becomes one entry per connection. `FireAllClients` goes to every connection, `FireClient(player)` to that player's, and `undelivered` counts only messages to players without a connection (the server-only extra players of `[compat] players` in an in-process run).
- A client over its backlog window (§1) gets no new ops that tick; its mirror still holds what it was last sent, so the next pass sends the coalesced difference. Remote messages and replies are never coalesced: if a client's reliable backlog passes 4 MiB (it stopped acking), the server closes the connection (`RE8906`) instead of buffering without bound.
- The defaults (`[net]`: 256 KiB/s per client down, 64 KiB/s up, `max_players = 16`) leave room for the measured plaza (31 to 48 KB/s per walking player at 60 Hz, a 23 KB join) and for games several times heavier.

**5. Join and leave.**
- **Join.** After Accept, the client sends `Join { game, name }`. `game` identifies the build: the hash of WP19's archive index for a packaged game, or of the project's content (scenes, scripts and `real.toml`, as `real verify` already reads them) for a project directory. A mismatch is `RE8905`, with both hashes in the message, because a client running different scripts or scenes would desynchronise in ways nobody could debug.
- The server makes the `Player`: its `Name` and `DisplayName` from `name` (`--name`, default `Player<n>`), made unique with a suffix, and a `UserId` of `-1`, `-2`, … in join order, as a vendor local test server does. It answers **Welcome** (the server tick, the player's handle, the component table), then sends the snapshot as ordinary create ops in tree order, under the same budget and backpressure as any frame, and ends it with `SnapshotDone`. The client runs `ReplicatedFirst` scripts as soon as Welcome arrives and the rest of its scripts after `SnapshotDone`, as M2's client does after the split.
- **Leave.** A Disconnect, a timeout or `Player:Kick(message)` removes the player: `PlayerRemoving` fires, its character is destroyed, its owned instances return to the server, and the kicked client shows the message and exits its run with `RE8907`.
- A player who joins a game already in progress gets the server's current state; nothing about the snapshot assumes the game just started.

**6. Running it.**
- `real run --server [--port P] [--bind ADDR] [--max-players N]` runs a **dedicated server**: headless, no window, no GPU device (`App` opens one only on first use, and a server never uses one), no audio device, stepping in real time at the fixed step. It is the runtime-only `real` binary, so a packaged game (WP19) can run as its own server.
- `real run --connect HOST:PORT [--name NAME]` runs a **client**: a window as usual, or `--headless` with `--replay` for CI. It has no server world, and its local player is the one the server gives it.
- `real run --host [--port P]` runs a **listen server**: today's in-process two worlds, with the local player as the first connection, accepting remote clients too. Play-testing in the editor stays in-process and offline.
- `real.toml` gains `[net]` (`port`, default 47800; `max_players`; the two rates; `timeout_ms`, default 10,000) and `[compat.net]` (`upstream`, `max_owned_speed`, `remote_rate`). A command-line flag overrides the file.
- A server binds `0.0.0.0` by default, because its purpose is other machines, and prints the addresses it listens on.

**7. Checking it: a lockstep mode for CI.**
- With `--lockstep` on both ends, the server runs tick N only when every connected client's frame N−1 has arrived, and a client runs frame N only when the server's frame N has arrived. Each side then sees exactly what the in-process run sees, in the same order: the client's owned changes from frame N−1 apply in the server's replication step of frame N, and its remotes from frame N−1 in the server's pump of frame N. So a networked lockstep run's server `state_hash` must equal the in-process run's hash for the same replay.
- `real run --server --lockstep --frames F --json` prints the server's run report with its `state_hash` per checkpoint frame, and `real verify` gains a `net` stage that starts the server and one client per replay as child processes over loopback and compares their hashes with the in-process stage.
- Ordinary play never waits; lockstep is for tests and CI only, and a lockstep server whose clients stop sending fails the run after the timeout instead of hanging.

**8. Scope in M3.**
- **In M3:** compat (Roblox-style) projects over UDP, with dedicated, listen and client runs, on the three desktop platforms.
- **Not in M3:** encryption and authentication; NAT traversal, relays, matchmaking and server lists (players connect to an address); client-side prediction and lag compensation beyond what network ownership gives; interest management (every client gets every replicated container; `StreamingEnabled` stays a stored property); and a **native networking API** for the engine's own `game` scripts. Native scripts have no server and client contexts, and designing them is its own ADR; `re_net`'s Rust API is public and documented, so a Rust game can use the transport directly in the meantime.
- **PR "web"** (in M3: ADR-0049 §10 put online play in the browser build): a WebSocket transport for the browser client and a WebSocket listener on the native server. It would add one dependency (a WebSocket crate that needs no async runtime, chosen in that PR's ADR amendment), and nothing in §2 to §7 changes.

**Consequences.**
- A Roblox-style game in M3 can be played by several people on different machines, with the server authoritative for everything but each player's own character, as on the vendor's platform, and with the same scripts as a single-process play test.
- No dependency is added in M3's core path, and the runtime-only build stays free of tokio. We own a transport of about 1,500 lines with tests (the spike is 600), which is the cost of that.
- The traffic is not encrypted. That is acceptable for LAN play and trusted hosts and is stated wherever the feature is documented; encryption is the first networking item for M4.
- M2's compat tests start exercising the wire code, because the in-process run uses it. The replication pass changes structure (diff and apply), which is the riskiest part of WP30 and is why it is a PR of its own before any socket code is used by a game.
- The private samples are measured in phase 1 before the budgets are frozen; if one is heavier than the defaults allow, the defaults change in PR "sessions" with the numbers in `research/`.
- The cookie key is a per-process random key (std's `RandomState` or 16 bytes from the OS), never a constant, so a Challenge cannot be precomputed.
- `docs/ARCHITECTURE.md` gains `re_net` in §2 (a subsystem crate), the range `RE8900`–`RE8999` in the code table, and a networking section; `docs/M3_PLAN.md` loses the "not in M3" default and WP27's networking non-goal, and gains WP30.

**Amendment (as built, WP30 PRs "transport" #90 and "wire" #117).**
- **§1, CRC-32 trailer.** Every packet ends with a CRC-32 of its bytes, because the UDP checksum is optional on IPv4 and a damaged datagram could otherwise be delivered corrupted or stall the reliable stream. With the 4-byte trailer and the chunk header as built, a reliable fragment carries 1,162 bytes (`re_net`'s `FRAGMENT`: 1,200 less the 15-byte header, the 8-byte data header, the 11-byte chunk header and the trailer), not 1,170.
- **§1 and §4, backlog.** A reliable send past `max_backlog` (4 MiB by default, with a fixed 64-byte cost per message) is refused with `RE8908`, a code added to `RE8900`–`RE8907`, instead of closing the connection, which would cut off a healthy connection whose game queued a large snapshot at once. A connection closes when its sent data sees no ack for the 10 s timeout, or after 10 s of silence (`RE8906`).
- **§1, cookie epochs.** The cookie also hashes a 30-second epoch, so a captured Response stops working after 30 to 60 s.
- **§3, remote messages carry instances by stable id**, not by handle; the id is resolved when the message is delivered, which keeps message semantics exact, including for messages that wait for their instance. Replication ops still use handles.
- **§3, ownership.** A change of ownership reaches the client as an `Own` op, so it takes effect one pass later. The server still checks ownership on every upstream write.
- **§3, the split pass** is `re_compat::net`'s `Conn` on the server (`bind`, `apply_up`, `diff`) and the `Replica` resource on the client (`diff_up`, `apply`), over `net::wire` (`Frame` with `encode` and `decode`, `WIRE_VERSION`, `MAX_DEPTH`). In-process frames go through encode and decode as bytes, not through a `MemoryNet` endpoint; the two-client test uses real endpoints. Decoding refuses nesting deeper than 128 levels, and a connection numbers at most 65,536 names.

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
