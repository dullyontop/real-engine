# Roadmap

There are four stages. Each milestone has exit criteria that can be checked by a command, not by opinion. The work breakdowns are in [M1_PLAN.md](M1_PLAN.md), [M2_PLAN.md](M2_PLAN.md) and [M3_PLAN.md](M3_PLAN.md).

## Ordering rationale

We order work by three questions, in this order:

1. **Importance to the owner's goals.** Procedural worlds, Luau scripting, agent-driven development, and eventually running Roblox-style projects.
2. **Dependency.** What other work is blocked until this exists.
3. **Feasibility.** How well a spike has already proven it.

The result:
- **M1 builds the core loop end to end**: render, physics, scripting, procedural generation, commands and verify. It uses an engine-native API. Every later feature plugs into these seams, and every one of them was proven by a spike, the entity core included.
- **M2 is compatibility.** It matters a great deal to the owner, but it depends on a working scripting runtime, object model, physics and frame order. It is also the largest chunk of work. The owner chose "Demo first" (DECISIONS, open question 3): M1 ships the demo on the engine's own API, and M2's goal is the sample scenes running through the Roblox-style layer; whether a mechanical migration step is acceptable on the way was left open, and M2 ended up running the samples from their unmodified source. Measuring the gap on real scripts (the M1 validation suite) comes before building the layer.
- **M3 adds scale, polish and the rest of a complete game workflow.** That means shipping games, richer rendering, cross-machine determinism, streaming, the node canvas and gizmos. None of these block the others, and each needs M1's measurements (PerfReport, D1 harness) to be done well.
- **AI integration grows in every milestone** (see "AI track" below), always on top of the same command registry, and always optional.

## M0: Spikes (done)

| Spike | Result | Report |
|---|---|---|
| Render | wgpu 30.0.1, winit 0.30.13 and egui 0.36.2, verified headless on lavapipe and llvmpipe and windowed under Xvfb. M1 WP1 ported the prototype into `crates/re_render/` and removed it. | `docs/research/rendering.md` |
| Physics | oxijolt 1.1.0 (Jolt 5.6.0): 12 scenarios pass; a 1000-body pile steps in 2.85–3.78 ms; builds from source in about 55 s | research bundle |
| Platform | Noise and RNG (golden on three architectures), glTF import, asset cache, file watching, kira audio with a headless fallback, input action map, logging, profiling choice | research bundle |
| AI | MCP server on rmcp 3.5.1 over stdio, 12 tests across two protocol revisions, driven end to end by Claude Code; the command and transaction pattern; agent-safety research | research bundle |
| Scripting | mlua 0.12.2 (Luau 0.740): sandbox, per-script environments, raw-interrupt CPU budget, memory limit, project-confined `require`, entity handles, scheduler, hot reload; 14 feature tests in three builds, benchmarks; type checking with luau-lsp 1.70.1 or a patched luau-analyze against a generated definitions file. Design-phase probes also compiled and parsed an external Roblox-style corpus with no failures. | research bundle; published as `docs/research/scripting.md` in M1 WP3 |
| Entity core | bevy_ecs 0.20.0 standalone, chosen over hecs, sparsey, shipyard and flecs on one shared workload. A prototype with our own component registry (by-name access, script-defined components that are real ECS components), stable string ids, JSON scenes that round-trip byte for byte, atomic edit transactions with exact undo/redo and a log, and procedural sources with overrides: 15 tests pass. At 100,100 entities: 173 ms to spawn, 0.42–1.36 ms per incremental transform update, 6.9 ms to despawn a 1,001-entity subtree and 10.2 ms to undo it. Rejected: bevy_reflect (glam 0.33 lock, no f64 → f32 coercion, a panicking `apply`) and RON (quadratic parse). | research bundle; published as `docs/research/core.md` in M1 WP0 |

All M0 spikes are done.

**Exit (met):** the stack versions are verified, every subsystem choice has spike evidence (ADR-0005 included), and the design documents in `docs/` are written.

## M1: Core vertical slice (done)

**Scope.** See M1_PLAN for the nine work packages:
- the workspace, CI, the contracts and the entity core: reflection, the scene and commands, ported from the core spike (WP0);
- render (WP1);
- physics (WP2);
- Luau scripting with the native API and the validation suite (WP3);
- the geometry and ProcGraph procedural core (WP4);
- assets, input, audio and animation-lite (WP5);
- runtime integration with `real` and the island demo (WP6);
- the agent and MCP (WP7);
- editor-lite (WP8).

**Exit criteria** (the full list is in M1_PLAN, "Definition of done"):
- `real verify --project examples/island --json` exits 0 headless: schemas, compile, boot, three input replays with identical `state_hash`, goldens, and PerfReport.
- The Xvfb smoke run prints `RUN OK`.
- The island can be played to its goal (collect everything, restart), and it can be regenerated with a new seed during play: generated entities whose StableKey survives keep their `Entity` and their overrides, collected items stay collected, and the character is re-grounded.
- The MCP end-to-end tests pass for both supported protocol revisions, and the policy blocks out-of-scope writes.
- The validation suite skips cleanly without `REAL_SCENE_SCRIPTS`. With it set, tier-1 compile passes for every file of the external corpora.
- `docs/research/scripting.md` and `docs/research/core.md` (the published spike reports) are in the repository.
- Linux CI passes on every required job. Windows and macOS jobs (build, plus tests of the pure-Rust crates) are reported; for WP0's scope they pass.

## M2: Roblox-style compatibility (done)

The work packages (WP9–WP17), contracts and definition of done are in [M2_PLAN.md](M2_PLAN.md).

**Scope.**
- `re_compat`, the compatibility layer described in [ARCHITECTURE.md §4](ARCHITECTURE.md#m2-roblox-style-compatibility-layer-re_compat):
  - the instance tree over entities (`ChildOf` / `Children`, with `Parent =` and `Destroy()` as the `reparent` and `despawn` operations);
  - a class layer over components: a class is a marker component, a component set and property aliases, with inheritance;
  - `CFrame`, `Vector3`, `Color3`, `UDim2`, `Enum`;
  - signals, including property-change signals (per-component change ticks plus a per-field diff) and child or descendant signals (observers on `ChildOf` insert and discard; `ChildOf` already owns its component hooks as a bevy relationship, so we cannot add hooks to it);
  - `RunService` events mapped to the engine phases;
  - the `task` library, including `task.wait` inside user coroutines;
  - Humanoid and character;
  - collision groups;
  - attributes and tags, which need container field types in `re_reflect` (a string-keyed map and lists; ADR-0032), and a tag index kept by hooks;
  - in-process remotes between `server` and `client` contexts;
  - clear `RE9002` errors for online-only services;
  - a compatibility definitions file for the type checker.
- The `server` and `client` script contexts.
- Luau-defined commands in the `tools` context, capped at the `edit` tier.
- Static cells for large static maps (ADR-0031), measured before they are accepted.
- An importer from an exported Roblox-style project (its script folders) into a Real Engine project.
- Optionally, a `real migrate` tool, if the owner accepts a mechanical migration step (the open part of question 3).

**Ordering inside M2.** The static gap report from M1's suite ranks API members by how often real scripts use them. They are implemented in that order, so coverage of real code rises fastest.

**Exit (met, 2026-10-20).** M2_PLAN's [definition of done](M2_PLAN.md#definition-of-done-for-m2) replaced the criteria drafted here, and every item of it holds:
- Validation tier 4 (boot) passed: each external sample project loads through the importer and runs 600 headless frames with no uncaught script error, with every unsupported member reported as `RE9001` or `RE9002`, not as a crash or a hang.
- The static gap report over the samples' live code shows no `missing` member, against `coverage.json`.
- The frame-order test confirms the phase mapping of render-step, stepped and heartbeat events.
- A 250,000-part synthetic static map meets the budgets M2_PLAN set, and ADR-0031 records the numbers.
- The sample projects run through the Roblox-style layer from their unmodified source and pass their scripted playthroughs, so no `real migrate` step was needed, and none is built.

## AI track

AI support stays optional and provider-neutral (ARCHITECTURE §6). It grows with the engine:

| Milestone | Adds | Built on |
|---|---|---|
| M1 | The command registry as MCP tools (25, including `scene.apply` for atomic multi-operation edits), dry runs, `real verify --json`, `real explain`, `engine.d.luau`, checkpoints, the policy file, generated `AGENTS.md` and per-client configs | `re_command`, `re_agent`, `re_mcp` |
| M2 | Luau-defined tools (`tools` context, capped at `edit`); MCP resources for the docs, the explain pages and `engine.d.luau` (for clients that support resources; everything stays reachable through tools); a verify-diff loop that reports which verify results a change altered; structured output schemas on every tool | the registry, ADR-0026 |
| M3 | Natural-language starting points for beginners: project templates and generator recipes that an agent fills in through ordinary commands; an evaluation harness that runs scripted agent tasks against sample projects and scores them with `real verify`; the HTTP MCP transport (WP26: templates and recipes #104, `real agent eval` #124, `real mcp --http` #92) | the registry, `real verify`, templates |

The engine never calls a model itself. Every capability an agent uses is also usable by a human through the CLI or the editor.

## M3: Shipping games, browser builds and online play (done)

The work packages (WP18–WP30), contracts and definition of done are in [M3_PLAN.md](M3_PLAN.md).

**What became of the items this section listed before M3 was planned** (each has its ADR):

| Item | At the end of M3 |
|---|---|
| Export and packaging | Built (WP19, ADR-0037): `real build` packs `game.rpak` with a runtime-only player into a Linux folder, a Windows folder or a macOS app, and the release workflow builds the prebuilt runtimes for a tagged release. |
| Render thread | Measured and not built (ADR-0038's amendment for #160): render submission is not the bottleneck. |
| Post-processing, cascaded shadows, GPU-driven rendering | The HDR post chain and cascaded sun shadows are built (WP20, ADR-0038). GPU-driven rendering is not; it stays later work. |
| Texture compression (BC/ASTC) and shader hot reload | BC textures are encoded at import (#164, ADR-0047); ASTC is not used, since the targets are desktops. Shader hot reload is built (#169, ADR-0038). |
| Skinned animation and blend trees | Built as glTF skins and animation graphs (WP21, ADR-0039). |
| Audio effects graph | Built (WP22, ADR-0041). |
| D2 determinism | Built (WP18, ADR-0036): replays end with the same `state_hash` on Linux, Windows and macOS. |
| World streaming and large terrains | Built (WP23, ADR-0042), with level-of-detail terrain, regional erosion and a floating origin. |
| Procedural workflow extensions | Built (WP24, ADR-0043): bake, structural overrides, generator versions and Luau generators. Streamed cells generate asynchronously (ADR-0042). |
| Entity-core extensions | `#[derive(Reflect)]` (#149), coalesced drags (#133) and entity notes in scene files (#147) are built; the typed fast path for bulk spawns was measured and deferred (ADR-0016's amendment). |
| Terrain erosion, GPU or streamed procedural evaluation | Erosion and streamed evaluation are built (ADR-0042). GPU evaluation is not; it stays later work. |
| Node canvas for ProcGraph | Built (WP25, ADR-0044). |
| Luau nodes inside ProcGraph | Built (WP24, ADR-0043). |
| Gizmos and docking in the editor | Built (WP25, ADR-0044). |
| Polygon booleans and roads | Built on i_overlay (WP24, ADR-0043). |
| Vehicles and ragdolls | Built (WP22, ADR-0040). |
| Script debugger | Built (WP25, ADR-0045). |
| In-process type checking (vendored Luau Analysis) | Not built: `real check --types` runs luau-lsp on all three platforms (DECISIONS, open question 2). |
| Windows and macOS blocking merges in CI | Built (WP18, ADR-0023's amendment). |

M3 also added browser builds (WP29, ADR-0049), online play for Roblox-style projects (WP30, ADR-0050), the compat follow-ups M2 deferred (WP27, ADR-0048), the M3 AI track (WP26, ADR-0046) and `examples/valley` (WP28).

**Left for later.** No milestone after M3 is planned yet. Besides GPU-driven rendering and GPU procedural evaluation (above), the ADRs leave these for later work: the typed fast path for bulk spawns (ADR-0016's amendment); type checking inside the engine in place of luau-lsp (DECISIONS, open question 2); a vehicle tyre curve with friction at zero slip, so that a car under throttle no longer stays in first gear until about its rpm limit (ADR-0040's amendment for #181); and a size limit for `re_proc`'s disk cache, which keeps every field and mesh it writes: a fresh `real verify` of the valley leaves about 770 MB in `.real/cache/proc`, a packaged game keeps the cache in the player's cache folder, and a browser build keeps it in memory (ADR-0049 §6).

**Exit (met, 2026-10-26).** M3_PLAN's [definition of done](M3_PLAN.md#definition-of-done-for-m3) replaced the minimum drafted here, and every item of it holds:
- The required jobs (ADR-0023), `build-windows`, `build-macos`, `d2`, `verify-plaza` and `verify-valley` among them, block merges and pass on Linux, Windows and macOS.
- The D2 harness gives the same `state_hash` for the island, plaza and valley replays on Linux x86_64, Windows x86_64 and macOS arm64.
- `real verify` passes every stage, goldens included, on the valley, the island and the plaza, on Linux, Windows and macOS.
- `real build` packages the valley for each platform, and the packaged game replays with the project's `state_hash`.
- Every ADR the plan lists is Accepted, and ADR-0038 records with measurements why there is no render thread.
- The M1, M2 and WP26 MCP end-to-end tests pass over stdio and HTTP.
- The island and the plaza build for the web and play their replays in headless Chromium with WebGPU, and the plaza's `net` stage passes on all three platforms.
- The docs were checked against the code at the end of M3 (the "maintainer audit" amendments in DECISIONS.md), and `coverage.json` matches the code.
