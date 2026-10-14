# Roadmap

There are four stages. Each milestone has exit criteria that can be checked by a command, not by opinion. The detailed M1 work breakdown is in [M1_PLAN.md](M1_PLAN.md).

## Ordering rationale

We order work by three questions, in this order:

1. **Importance to the owner's goals.** Procedural worlds, Luau scripting, agent-driven development, and eventually running Roblox-style projects.
2. **Dependency.** What other work is blocked until this exists.
3. **Feasibility.** How well a spike has already proven it.

The result:
- **M1 builds the core loop end to end**: render, physics, scripting, procedural generation, commands and verify. It uses an engine-native API. Every later feature plugs into these seams, and every one of them was proven by a spike, the entity core included.
- **M2 is compatibility.** It matters a great deal to the owner, but it depends on a working scripting runtime, object model, physics and frame order. It is also the largest chunk of work, and its size depends on an open owner question (DECISIONS, open question 3). Measuring the gap on real scripts (the M1 validation suite) comes before building the layer.
- **M3+ adds scale, polish and the rest of a complete game workflow.** That means shipping games, richer rendering, cross-machine determinism, streaming, the node canvas and gizmos. None of these block the others, and each needs M1's measurements (PerfReport, D1 harness) to be done well.
- **AI integration grows in every milestone** (see "AI track" below), always on top of the same command registry, and always optional.

## M0: Spikes (done)

| Spike | Result | Report |
|---|---|---|
| Render | wgpu 30.0.1, winit 0.30.13 and egui 0.36.2, verified headless on lavapipe and llvmpipe and windowed under Xvfb. The prototype is kept in `prototypes/render/`. | `docs/research/rendering.md` |
| Physics | oxijolt 1.1.0 (Jolt 5.6.0): 12 scenarios pass; a 1000-body pile steps in 2.85–3.78 ms; builds from source in about 55 s | research bundle |
| Platform | Noise and RNG (golden on three architectures), glTF import, asset cache, file watching, kira audio with a headless fallback, input action map, logging, profiling choice | research bundle |
| AI | MCP server on rmcp 3.5.1 over stdio, 12 tests across two protocol revisions, driven end to end by Claude Code; the command and transaction pattern; agent-safety research | research bundle |
| Scripting | mlua 0.12.2 (Luau 0.740): sandbox, per-script environments, raw-interrupt CPU budget, memory limit, project-confined `require`, entity handles, scheduler, hot reload; 14 feature tests in three builds, benchmarks; type checking with luau-lsp 1.70.1 or a patched luau-analyze against a generated definitions file. Design-phase probes also compiled and parsed an external Roblox-style corpus with no failures. | research bundle; published as `docs/research/scripting.md` in M1 WP3 |
| Entity core | bevy_ecs 0.20.0 standalone, chosen over hecs, sparsey, shipyard and flecs on one shared workload. A prototype with our own component registry (by-name access, script-defined components that are real ECS components), stable string ids, JSON scenes that round-trip byte for byte, atomic edit transactions with exact undo/redo and a log, and procedural sources with overrides: 15 tests pass. At 100,100 entities: 173 ms to spawn, 0.42–1.36 ms per incremental transform update, 6.9 ms to despawn a 1,001-entity subtree and 10.2 ms to undo it. Rejected: bevy_reflect (glam 0.33 lock, no f64 → f32 coercion, a panicking `apply`) and RON (quadratic parse). | research bundle; published as `docs/research/core.md` in M1 WP0 |

All M0 spikes are done.

**Exit (met):** the stack versions are verified, every subsystem choice has spike evidence (ADR-0005 included), and the design documents in `docs/` are written.

## M1: Core vertical slice

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
- `real verify --project examples/island --json` exits 0 headless: schemas, compile, boot, two input replays with identical `state_hash`, goldens, and PerfReport.
- The Xvfb smoke run prints `RUN OK`.
- The island can be played to its goal (collect everything, restart), and it can be regenerated with a new seed during play: generated entities whose StableKey survives keep their `Entity` and their overrides, collected items stay collected, and the character is re-grounded.
- The MCP end-to-end tests pass for both supported protocol revisions, and the policy blocks out-of-scope writes.
- The validation suite skips cleanly without `REAL_SCENE_SCRIPTS`. With it set, tier-1 compile passes for every file of the external corpora.
- `docs/research/scripting.md` and `docs/research/core.md` (the published spike reports) are in the repository.
- Linux CI passes on every required job. Windows and macOS jobs (build, plus tests of the pure-Rust crates) are reported; for WP0's scope they pass.

## M2: Roblox-style compatibility

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
- Optionally, a `real migrate` tool, depending on the owner's answer about running scripts unchanged.

**Ordering inside M2.** The static gap report from M1's suite ranks API members by how often real scripts use them. They are implemented in that order, so coverage of real code rises fastest.

**Exit criteria.**
- Validation tier 4 (boot) passes: each external sample project loads through the importer and runs 600 headless frames with no uncaught script error, and with every unsupported member reported as `RE9001` or `RE9002`, not as a crash or a hang.
- The static gap report shows no `missing` member with more than a threshold number of uses. The owner sets the threshold when M2 starts.
- Frame-order tests confirm the phase mapping of render-step, stepped and heartbeat events.
- A large synthetic static map loads, and its PerfReport stays inside budgets. The owner sets the map size and the budgets at the start of M2.
- Whether the sample projects run *unchanged* or after `real migrate` follows the owner's decision. The exit criteria are rewritten once that decision is made.

## AI track

AI support stays optional and provider-neutral (ARCHITECTURE §6). It grows with the engine:

| Milestone | Adds | Built on |
|---|---|---|
| M1 | The command registry as MCP tools (25, including `scene.apply` for atomic multi-operation edits), dry runs, `real verify --json`, `real explain`, `engine.d.luau`, checkpoints, the policy file, generated `AGENTS.md` and per-client configs | `re_command`, `re_agent`, `re_mcp` |
| M2 | Luau-defined tools (`tools` context, capped at `edit`); MCP resources for the docs, the explain pages and `engine.d.luau` (for clients that support resources; everything stays reachable through tools); a verify-diff loop that reports which verify results a change altered; structured output schemas on every tool | the registry, ADR-0026 |
| M3+ | Natural-language starting points for beginners: project templates and generator recipes that an agent fills in through ordinary commands; an evaluation harness that runs scripted agent tasks against sample projects and scores them with `real verify`; the HTTP MCP transport | the registry, `real verify`, templates |

The engine never calls a model itself. Every capability an agent uses is also usable by a human through the CLI or the editor.

## M3 and later

These are not ordered yet. Each one needs an ADR before it starts.

| Item | Why later | Seam already in place |
|---|---|---|
| Export and packaging (a game build without tooling, assets packed, per-platform bundles) | M1 needs the engine to run from a project directory first | `re_cli --no-default-features` links no tooling (ADR-0027); `.real/cache` processed assets |
| Render thread | M1 PerfReport must show that render submission is the bottleneck first | `RenderSnapshot: Send` (ADR-0007) |
| Post-processing (tone mapping, bloom), cascaded shadows, GPU-driven rendering | The M1 forward pass must be measured first | `Renderer::render` takes a snapshot; passes are internal to `re_render` |
| Texture compression (BC/ASTC) and shader hot reload | Not needed for the M1 demo | `re_assets` processed cache; the WGSL `#include` preprocessor |
| Skinned animation and blend trees | Not needed for the M1 demo | `re_types::ClipData`, `re_anim` sampler |
| Audio effects graph (reverb, filters, buses) | M1 needs playback only | `AudioCmd`; kira's effect support |
| D2 determinism (identical across machines and operating systems) | Expensive: oxijolt `cross-platform-deterministic` plus a float audit | D1 harness, `state_hash`, sorted commands (ADR-0018) |
| World streaming and large terrains | Needs static cells and measurements from M2 | `StableKey`, per-node cache, fragments |
| Procedural workflow extensions: bake to authored entities, structural overrides beyond deletion, generator versioning, asynchronous generation, Luau generators | The source-plus-overrides model must settle first | `GenNode` is plain data; `Generators` registry (ADR-0017) |
| Entity-core extensions: a `#[derive]` component macro, undo coalescing for drags, a typed fast path for bulk spawns and scene loads, comments in scene files | Not needed for M1; each is an open question of the core spike | `component!`, `Transaction` labels, `SceneDoc` (ADR-0016, ADR-0032) |
| Terrain erosion, GPU or streamed procedural evaluation | The CPU node set and its cache must settle first | `NodeRegistry`, per-node keys, `Field` outputs |
| Node canvas for ProcGraph | The IR and commands must settle first | `GraphDoc` and the `graph.*` commands |
| Luau nodes inside ProcGraph | Needs the scripting performance numbers on node-sized work | `NodeRegistry`; the `GraphDoc` boundary between `re_script` and `re_proc` |
| Gizmos and docking in the editor | UI polish | Every editor action is already a command |
| Polygon booleans and roads | `i_overlay` was too new to adopt when we checked | Node registry |
| Vehicles and ragdolls | Not needed for the M1 demo. oxijolt 1.1.0 has `vehicle` and `ragdoll` modules (seen in its source; not exercised by the spike) | `re_physics` hides oxijolt; `PhysicsCmd` |
| Script debugger | `set_hook` does not exist in Luau builds; a design on the interrupt callback and `inspect_stack` needs a spike | `re_script` contexts, the raw interrupt |
| In-process type checking (vendored Luau Analysis) | Owner decision (DECISIONS, open question 2) | `re_script_check::types`, which already returns `Diagnostic`s |
| Windows and macOS blocking merges in CI | Unverified today, including the raw Luau interrupt on MSVC | Windows and macOS jobs already build the workspace and test the pure-Rust crates on every PR (ADR-0023) |

**M3 exit criteria** are defined when M3 is planned. The minimum: Windows and macOS block merges in CI, and the D2 harness passes across Linux x86_64 and one other platform.
