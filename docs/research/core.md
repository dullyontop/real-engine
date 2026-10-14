# Core spike: entities, components, reflection, scenes and commands

Real Engine research spike, 2026-10-14. Everything marked **verified** came from
code that compiled and ran in this container (Linux, 4 shared CPUs, no GPU,
rustc/cargo 1.97.0). Anything else is marked **unverified**. The prototype was
ported into this repository as `crates/re_reflect`, `crates/re_scene` and
`crates/re_command` (M1 WP0, §12 maps the files). The spike's raw outputs,
run logs and throwaway comparison crates are not in this repository; they are
summarised here, and the measurements below come from those runs.

## 1. Recommendations

| Decision | Recommendation | Confidence |
|---|---|---|
| Entity/component storage | **bevy_ecs 0.20.0 used standalone**, features `std` + `multi_threaded`, pinned with `=` and hidden behind the engine's own core API | High |
| Reflection | **In-house component registry** (a `component!` macro now, a derive macro later) over typed bevy_ecs components, plus **runtime-declared script components** that are real bevy_ecs components. Do **not** use bevy_reflect. | High |
| Scene format | **JSON** (serde_json with `float_roundtrip`), schema-driven, flat entity list, scalar lists kept on one line, `deny_unknown_fields`, plus an emitted JSON Schema. RON is the runner-up; TOML is rejected. | Medium-high |
| Identity | **Stable string ids** in files, commands, logs and agent conversations. Runtime `Entity` handles are never persisted. An id index is kept in sync by component hooks. | High |
| Hierarchy | bevy_ecs's built-in `ChildOf` / `Children` relationship (ordered, recursive despawn) plus our own change-driven transform propagation | High |
| Editing | One **command system** (spawn, despawn, set_field, add/remove_component, reparent, rename) shared by the editor, scripts and agents. It is atomic per transaction, gives exact inverses for undo/redo, and keeps an append-only JSON-lines log. | High |
| Procedural content | **Source entity plus parameter component plus overrides.** Generated entities have deterministic keys and ids, are never saved, are rebuilt when the inputs' fingerprint changes, and have user edits re-applied by key. | Medium-high: the model works, but Bake, prefabs and Luau generators are not built yet |

### Why these choices
- **bevy_ecs** is the only Rust option tested that has everything we need already built: hierarchy, hooks, observers, per-component change ticks, dynamic (runtime-defined) components with by-id access, and a parallel scheduler.
  - hecs and sparsey are faster to compile but would need all of that written in-house.
  - flecs has the features, but it is C, it segfaulted from a safe Rust call, and it spawned about 4× slower.
- **bevy_reflect** has four problems for us:
  - It ties the engine to bevy's glam version. That is 0.33.12, while the wgpu ecosystem we verified uses 0.34.1, and bevy moves glam every release.
  - It does no numeric coercion: a Luau f64 into an f32 field is an error, and `ReflectComponent::apply` panics on it.
  - It writes full Rust type paths into files.
  - It has renamed or moved its APIs in every release.

  Our registry is about 400 lines. It speaks a closed set of property types that map one-to-one onto Luau values, JSON and Roblox-style typed properties.
- **JSON over RON:**
  - Every AI model and tool already speaks JSON, JSON Schema can validate it, and it parsed 8.6× faster than RON in the typed format test.
  - We hit a quadratic-time parse in ron 0.12.2 on exactly the kind of dynamic value tree a schema-driven scene uses: 32 s for 4,000 entities.
  - RON's main advantage, readable diffs, is mostly recovered by keeping vectors on one line.

  What JSON still lacks is comments (see open questions).

## 2. Chosen crates (verified versions)

| Crate | Version | MSRV | License | Role |
|---|---|---|---|---|
| bevy_ecs | 0.20.0 (released 2026-10-08) | 1.95.0 | MIT OR Apache-2.0 | storage, hierarchy, hooks, change detection, schedule |
| glam | 0.34.1 | (none declared in its Cargo.toml) | MIT OR Apache-2.0 | math, matching wgpu 30 / the first spike |
| serde | 1.0.x (resolved 1.0.228) | — | MIT OR Apache-2.0 | |
| serde_json | 1.0.154, feature `float_roundtrip` | 1.71 | MIT OR Apache-2.0 | scene, commands, log, schema |
| indexmap | 2.14.2, feature `serde` | 1.85 | Apache-2.0 OR MIT | ordered maps in docs |
| ron | 0.12.2 | 1.64.0 | MIT OR Apache-2.0 | optional second format (prototype only) |

- The combined MSRV is **1.95** (set by bevy_ecs). That matches the first spike's wgpu/winit/egui floor.
- The prototype's dependency tree has 89 packages (`cargo tree -e normal,build`, deduplicated). Build times are in section 6.

## 3. Rejected options and why

### Entity storage
All numbers come from the comparison crates; full tables are in section 6.

| Option | Verdict | Why |
|---|---|---|
| hecs 0.11.2 | Rejected | Smallest and quickest to build (6 crates, 5.7 s CPU debug). It has no hierarchy, no hooks or observers, no per-component change ticks (only a clone-and-compare tracker), no scheduler, and no by-id byte access; runtime component types are only partly possible through `TypeInfo::from_parts`. We would rebuild most of bevy_ecs. |
| sparsey 0.13.4 | Rejected | Fastest structural churn (under 1.2 ms for 10k add+remove), but no dynamic components, no change detection, no hooks, no hierarchy and no scheduler. |
| shipyard 0.11.5 | Strong runner-up | Sparse sets, opt-in change tracking, insertion/removal hooks, a parallel `Workload` and serde support. But there is no built-in hierarchy, custom storages still need a Rust type (no by-id byte access found), and it is slower to iterate (about 450 µs vs 270 µs per 100k pass). |
| flecs_ecs 0.2.2 | Rejected | Feature-rich: hierarchy, reflection/meta with JSON, observers, runtime components. But it is a C library via bindgen. `query.set_cached().build().is_changed()` without `.detect_changes()` **segfaults from safe Rust**. Spawn is about 4× slower (100 ms per 100k) and random `get` about 2× slower. |
| bevy_ecs with default features | Not needed | It adds bevy_reflect, async_executor and backtrace (+12 crates, about +45% build CPU) with no benefit, given our own registry. |
| In-house storage | Deferred | Nothing in the requirements needs custom storage. bevy_ecs's table/sparse-set split, hooks and relationships are exactly what we would write, and the engine-facing API (`Core`, `Registry`, `Command`) hides the choice. |

### Reflection
| Option | Verdict | Why |
|---|---|---|
| bevy_reflect 0.20.0 | Rejected (verified in detail; see section 7) | glam 0.33 lock, no f64→f32 coercion, a panicking `apply`, full type paths as file keys, silent no-ops on enum shape mismatch, and API churn every release. bevy_remote's JSON schema export has no docs or defaults and pulls in 278–322 crates. |
| In-house registry | **Chosen** | Small, stable, purpose-built property types, our own error messages and coercion rules, and the same model for native and script-defined components. |

### Scene formats
Full data is in section 7.

| Format | Verdict | Why |
|---|---|---|
| TOML 1.1.8 | Rejected | Deep `[entities.components.X]` headers, no `None` inside arrays, struct fields reordered, a key appended at the end of a block silently moves into the last sub-table, and the slowest parser (1.6 s per 100k typed). |
| RON 0.12.2 | Runner-up, not chosen now | Most compact, comments and trailing commas, smallest diffs. But **`deserialize_any` is O(n²)** (see gotchas), it is 3–9× slower than JSON even when linear, and models write it less reliably. Revisit if ron fixes the lookahead and we want comments. |
| JSON | **Chosen** | Fast, universal, schema-validatable, and exact with `float_roundtrip`. Its weaknesses are no comments, no trailing commas, and NaN written as `null`; NaN is impossible here because the registry rejects non-finite floats. |

## 4. What the prototype does (all verified by 15 passing tests plus 3 examples)

The crate `re_core` (about 2,600 lines of library code, plus about 900 lines of tests and examples):
- **`value.rs`.** A closed set of field types: bool, i32, i64, f32, f64, vec3, quat, color, string, entity, and unit enums. It has one dynamic `Value` type and central coercion rules: int→float; integral float→int; f64→f32 with a range check; non-finite rejected; quaternions normalised; enums checked against their variants.
- **`registry.rs`.**
  - The `component!` macro declares a native component with field docs and defaults and implements `Reflect`.
  - `Registry::register_script` declares a component at runtime. Script components get a real bevy_ecs `ComponentId`, created with `ComponentDescriptor::new_with_layout`, that stores a `Vec<Value>`. They are queryable, change-tracked and saved like native ones.
  - Reads and writes go by name; an equal write does not trip change detection.
  - `references_to` finds every entity-typed field pointing into a set of entities.
- **`ids.rs`.** `StableId` is an immutable component. Its `on_insert` / `on_discard` hooks keep the `IdIndex` resource exact, through despawn of whole subtrees too. `IdGen` makes ids like `e7kq2m9x4tb`.
- **`hierarchy.rs`.** Helpers over `ChildOf` / `Children`, plus two propagation systems:
  - `propagate_all` recomputes everything.
  - `propagate_changed` handles only subtrees whose `Transform` or `ChildOf` changed, and also catches *removed* `ChildOf` via `RemovedComponents`. A test proves it equals the full recomputation.
- **`doc.rs`.** `Doc` is the untyped tree used at every text boundary; it is serde-generic, so the same code reads JSON and RON. It converts against the schema with good messages, and accepts `[x,y,z]` or `{x,y,z}` for vectors and 3 or 4 numbers for colours.
- **`scene.rs`.** Save and load, with deterministic order:
  - roots sorted by id, then depth-first with siblings in `Children` order;
  - components sorted by name, fields in declaration order.

  Loading is atomic: a failed load leaves the world untouched. Option `omit_defaults` halves file size. Scalar lists are collapsed onto one line.
- **`schema.rs`.** `Core::describe()` emits the registry as JSON (components, fields, types, defaults, docs, generators). `Core::scene_json_schema()` emits JSON Schema 2020-12 for scene files.
- **`commands.rs`.** The command set, transactions with rollback, undo/redo, and the log.
- **`procedural.rs`.** `Procedural` and `PillarGrid` components, the `Generated` marker, `Overrides`, a generator registry, fingerprint-driven regeneration, and the `pillar_grid` example generator.
- **`core.rs`.** `Core { world, registry, generators }` with `update()`, which regenerates changed procedural sources and then runs the propagation schedule.

### Verified behaviours (test name → what it proves)
- **`by_name_access_and_coercion`.** Reads and writes go by name. An f64 0.1 from a script becomes f32 0.1. Error texts are exact (see the cheat-sheet).
- **`script_components_are_real_ecs_components`.**
  - A runtime-declared `Health` gets a `ComponentId`, is visible through `contains_id`, and is set and read by name.
  - It saves to JSON and loads into a fresh world (after re-registering the schema) byte-identically.
  - It survives despawn and undo.
- **`id_index_follows_hooks`.** Despawning a 2-entity subtree drops both ids from the index; undo restores them.
- **`hierarchy_reparent_order_and_cycles`.** Reparent with an index reorders siblings, and undo restores the order. Cycles are rejected. Unparenting works.
- **`transform_propagation_incremental_matches_full`.** Rotating a parent moves a child to (-1, 5, 0). Unparenting is noticed. Incremental propagation equals full propagation.
- **`scene_round_trip_is_byte_identical`.** JSON → world → JSON is byte-identical, and so is RON → world → RON.
- **`double_rounding_f32_is_recovered`** and **`every_f32_survives_text_via_f64`**: see the f32 gotcha.
- **`load_errors_name_the_place`.** Bad files fail with entity index, id, component and field in the message. Unknown keys fail. Forward parent references fail. A failed load leaves zero entities.
- **`undo_redo_restores_identical_scenes`.** Four transactions (set, rename + remove component, despawn of a subtree, spawn with a generated id) are undone and redone. Each step's saved scene is byte-identical to the original at that step, including the redone generated id. An entity reference from *outside* a despawned subtree is restored by undo.
- **`failed_transaction_rolls_back`.** A 4-command transaction failing on command 3 leaves the scene byte-identical and logs a `rejected` entry.
- **`commands_are_plain_json_for_agents`.** Commands parse from JSON. The spawn result carries the generated id. The log is JSON lines with the origin (`{"agent":"claude"}`).
- **`procedural_sources_overrides_and_regeneration`.**
  - A 2×3 `pillar_grid` makes 6 pillars plus caps, and none of them is written to the file.
  - Editing `grid/pillar_0_0` records an override; undo removes both the colour and the override.
  - Changing `cols` regenerates and keeps the override.
  - The same key and seed give the same height whatever the grid size.
  - Shrinking to zero rows reports an orphaned override and keeps it.
  - Despawning a generated entity is refused with a hint.
  - Despawn and undo of the source rebuilds the output with the override applied.
- **`equal_writes_do_not_trigger_change_detection`.** An equal `set_field` leaves `Changed<Transform>` empty; a different value sets it.

## 5. API cheat-sheet (from the compiling code)

**Declare a native component** (`components.rs`):
```rust
component! {
    /// Position, rotation and scale relative to the parent entity.
    pub struct Transform requires(GlobalTransform) {
        /// Offset from the parent's origin, in metres.
        pub translation: Vec3 = Vec3::ZERO,
        pub rotation: Quat = Quat::IDENTITY,
        pub scale: Vec3 = Vec3::ONE,
    }
}
field_enum! { pub enum BodyKind { Static, Dynamic, Kinematic } }
registry.register::<Transform>(&mut world);
```

**Declare a component at runtime** (a script or an agent):
```rust
core.registry.register_script(&mut core.world, ComponentSchema {
    name: "Health".into(), doc: "Hit points.".into(), origin: Origin::Script,
    fields: vec![FieldSchema { name: "current".into(), ty: FieldType::F32, doc: String::new(), default: Value::Int(100) }],
})?;
```

Under the hood (`registry.rs`):
```rust
let descriptor = unsafe { ComponentDescriptor::new_with_layout(
    schema.name.clone(), StorageType::Table, Layout::new::<ScriptData>(),
    Some(drop_script_data), true, false, ComponentCloneBehavior::Ignore, None) };
let id = world.register_component_with_descriptor(descriptor);
OwningPtr::make(ScriptData(values), |ptr| unsafe { world.entity_mut(e).insert_by_id(id, ptr); });
let data = unsafe { world.get_by_id(e, id)?.deref::<ScriptData>() };
```

**Read and write by name:**
```rust
core.get("crate", "RigidBody", "mass")?;          // Value::Float(20.0)
core.get_doc("weld", "Weld", "other")?;           // Doc::Str("crate") — entity refs as ids
let key = core.registry.key("Transform")?;        // resolve once for hot paths
let fi = core.registry.schema(key).field_index("translation")?;
core.registry.set_field(&mut core.world, entity, key, fi, Value::Vec3(v))?;
```

**Commands as an agent sends them** (verified JSON):
```json
[{"op": "spawn", "id": "crate", "name": "Crate", "parent": "level",
  "components": {"Transform": {"translation": {"x": 0.1, "y": 2, "z": -3}},
                 "RigidBody": {"kind": "Dynamic", "mass": 12.5}}},
 {"op": "set_field", "id": "yard/pillar_1_0", "component": "Primitive", "field": "color", "value": [0.9, 0.1, 0.1, 1]},
 {"op": "reparent", "id": "crate", "parent": "yard", "index": 0},
 {"op": "despawn", "id": "crate"}]
```

```rust
let mut history = History::default();
let applied = history.execute(&mut core, Transaction { label, origin: Origin::Agent("claude".into()), commands })?;
history.undo(&mut core)?; history.redo(&mut core)?;
let new_entries = history.log_since_jsonl(last_seen_seq);
```

**Error messages an agent gets back** (verbatim from the spike's demo, ported as `crates/re_command/examples/agent_demo.rs`):
```
command 0: component RigidBody has no field "mas" (did you mean "mass"?); fields are: kind, mass, friction, restitution
command 0: unknown component "Rigidbody" (did you mean "RigidBody"?)
command 0: RigidBody.kind: unknown variant "Sleeping", expected one of Static, Dynamic, Kinematic
command 0: Transform.translation: expected vec3 as [x, y, z], found list of 2
command 0: cannot parent "level" under "crate": "crate" is "level" or one of its descendants
command 0: "yard/pillar_0_0" is generated: change its source's generator parameters instead
command 0: no entity with id "crates"
command 0: an entity with id "sun" already exists
command 0: entity already has a RigidBody component; use set_field to change it
unknown variant `set_feild`, expected one of `spawn`, `despawn`, `set_field`, `add_component`, `remove_component`, `reparent`, `rename`, `restore`, `set_override` at line 1 column 18
parse error: entities[0] (id "a"): Transform.translation: expected vec3 as [x, y, z], found list of 2
parse error: entities[0] (id "a"): parent "b" is not defined earlier in the file or in the world
```

**A scene file** (the demo's output; the yard's pillars are generated and not written):
```json
{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "level", "name": "Level" },
    {
      "id": "yard",
      "name": "Yard",
      "parent": "level",
      "components": {
        "PillarGrid": { "rows": 2, "cols": 2, "spacing": 6.5, "height": 6.0, "jitter": 1.0 },
        "Procedural": { "generator": "pillar_grid", "seed": 42 },
        "Transform": {
          "translation": [0.0, 0.0, 0.0],
          "rotation": [0.0, 0.0, 0.0, 1.0],
          "scale": [1.0, 1.0, 1.0]
        }
      },
      "overrides": [
        { "target": "pillar_1_0", "component": "Primitive", "field": "color", "value": [0.9, 0.1, 0.1, 1.0] }
      ]
    }
  ]
}
```
In the real output each key is on its own line (serde_json pretty), and only scalar lists are collapsed. This excerpt is compressed for space. The spike's full output files (`scene.json`, `scene.compact.json`, `scene.ron`, `registry.json`, `scene.schema.json` and `log.jsonl`) are not in this repository; `cargo run -p re_command --example agent_demo -- <dir>` writes the current `scene.json`, `registry.json`, `scene.schema.json` and `log.jsonl`.

**Hierarchy (bevy_ecs 0.20):**
```rust
world.entity_mut(parent).add_child(child);           // append
world.entity_mut(parent).insert_child(index, child); // also reorders an existing child
world.entity_mut(child).remove::<ChildOf>();         // make root
world.entity_mut(parent).despawn();                  // recursive (Children has linked_spawn)
```

**Hooks (0.20 names):**
```rust
#[derive(Component)]
#[component(immutable, on_insert = index_id, on_discard = unindex_id)]
pub struct StableId(pub String);
fn index_id(mut world: DeferredWorld, ctx: HookContext) { /* world.resource_mut::<IdIndex>() ... */ }
```

## 6. Measurements

### 6.1 Prototype at 100,100 entities (verified)
- **How it was run:** the spike's benchmark (ported as `crates/re_scene/examples/core_bench.rs`), release build, machine nearly idle (load average 1.8).
- **The world:** 100 roots × 10 blocks × 99 leaves. Each entity has a `StableId`, a `Name` and a `Transform` (plus the required `GlobalTransform`); leaves also have a `Primitive`.
- **Precision:** medians of 3 to 11 runs where shown. A second run under load gave numbers within about 1.5× of these.

**Spawning**

| Operation | Time |
|---|---|
| Direct typed bundles + `add_child`, including id-index hooks and string ids | 173 ms (1.7 µs per entity) |
| Through 100,100 `Spawn` commands in one transaction | 1.03 s (10 µs per entity) |

**Transform propagation**

| Operation | Time |
|---|---|
| `propagate_all`, full | 5.5 ms |
| `update()` incremental, nothing changed | 0.42 ms |
| `update()` after moving 1,000 leaves | 0.99 ms |
| `update()` after moving 10 roots (10,010 entities) | 1.36 ms |

**Field access (1 million operations)**

| Operation | Time |
|---|---|
| Typed `world.get::<Transform>` | 8.5 ms |
| `registry.get_field`, key resolved once | 15.4 ms |
| `core.get(id, "Transform", "translation")`, all by string | 107 ms (≈107 ns each) |
| Typed `get_mut` write | 15.2 ms |
| `registry.set_field`, with coercion and equality check | 36 ms |

**Saving and loading**

| Operation | Time / size |
|---|---|
| Save to `SceneDoc` | 299 ms |
| `SceneDoc` → JSON text | 632 ms; 40.5 MB, or 21.3 MB with `omit_defaults` |
| `SceneDoc` → RON text | 1.33 s; 53.1 MB |
| JSON text → `SceneDoc` | 477 ms |
| Load `SceneDoc` into the world | 486 ms |
| RON text → `SceneDoc` | not measurable (quadratic, see gotcha 2) |

The JSON round trip was byte-identical, and the benchmark asserts this.

**Undo**

| Operation | Time |
|---|---|
| Despawn command on a 1,001-entity subtree (snapshot + reference scan + despawn) | 6.9 ms |
| Undo of that despawn (restore 1,001 entities) | 10.2 ms |

What these numbers mean:
- Everything an editor or agent does interactively takes milliseconds at 100k entities.
- By-name access costs about 2× typed access with a pre-resolved key, and about 12× when every name is a string lookup. That is fine for scripts and agents; hot engine systems stay typed.
- Command-based bulk spawning is 6× slower than direct spawning (see open question 6).
- The full scene save/load path is about 1 s each way for 40 MB, dominated by the generic `Doc` tree. A typed fast path or `omit_defaults` would roughly halve it.

### 6.2 Build cost of the prototype (verified, near-idle machine)

| Build | Wall | CPU (user + sys) |
|---|---|---|
| Clean debug (`cargo build --lib`) | 37.9 s | 100.7 s |
| Clean release (`--lib`) | 49.6 s | 148.0 s |
| Incremental after touching one source file (lib) | 0.9 s | — |
| Incremental, tests | 1.5–3.0 s | — |

There are 89 packages in the tree, almost all from bevy_ecs with `multi_threaded`.

### 6.3 ECS comparison
- **Conditions:** separate crates, release build, a shared workload with N = 100,000, median of 5 in-process runs, reported as pass 1 / pass 2.
- **Load:** average 3–9 during these runs, so differences under about 20% are noise.
- **Checksums:** identical across all crates.

| Crate | Spawn 100k (ms) | Iterate per pass (µs) | Insert / remove Tag on 10k (ms) | 10k random get (µs) | Despawn all (ms) |
|---|---|---|---|---|---|
| hecs 0.11.2 | 22.4 / 22.8 | 282 / 283 | 4.0 / 1.05 | 1680 / 1372 | 5.4 / 5.2 |
| shipyard 0.11.5 | 29.1 / 27.4 | 485 / 449 | 0.68 / 0.31 | 416 / 363 | 14.1 / 13.2 |
| sparsey 0.13.4 | 28.0 / 26.8 | 308 / 303 | 0.96 / 0.14 | 193 / 169 | 8.2 / 7.4 |
| sparsey, grouped | 29.4 / 29.9 | 222 / 226 | 1.11 / 0.16 | 400 / 324 | 10.2 / 9.1 |
| **bevy_ecs 0.20.0 (std)** | 29.6 / 28.7 | 261 / 277 | 6.4 / 3.3 | 1220 / 1297 | 30.8 / 30.7 |
| bevy_ecs 0.20.0 (default features) | 27.4 / 31.1 | 264 / 295 | 6.9 / 2.9 | 1053 / 1242 | 28.6 / 27.8 |
| flecs_ecs 0.2.2 | 103 / 101.5 | 217 / 226 | 4.9 / 2.2 | 2822 / 3674 | 10.0 / 11.6 |

Build cost of each comparison crate (`cargo clean` then build; CPU = user + sys; noisy):

| Crate | Debug CPU s / wall s | Release CPU s / wall s | Packages in tree |
|---|---|---|---|
| hecs | 5.7 / 4.4 | 8.4 / 4.8 | 6 |
| sparsey | 5.5 / 5.3 | 7.9 / 7.3 | 6 |
| shipyard | 22.5 / 13.5 | 35.3 / 23.6 | 19 |
| bevy_ecs (std) | 66.9 / 50.7 | 98.9 / 66.6 | 58 |
| bevy_ecs (default features) | 97.5 / 75.3 | 163.6 / 115.2 | 70 |
| flecs_ecs | 95.0 / 67.4 | 104.3 / 82.0 | 44 |

MSRVs: hecs 1.81; sparsey and shipyard declare none; bevy_ecs 1.95.0; flecs_ecs 1.88. Licenses are MIT OR Apache-2.0, except flecs_ecs (MIT).

Feature matrix: only bevy_ecs and flecs had everything. The citations were in the spike's comparison logs, which are not in this repository.

| Feature | bevy_ecs | hecs | shipyard | sparsey | flecs |
|---|---|---|---|---|---|
| Runtime-defined components | yes (run) | partial (src) | partial (src) | no | yes (run) |
| By-id byte access | yes (run) | no | not found | no | yes (run) |
| Change detection | per component (run) | clone-compare | per component, opt-in | none | per table |
| Hierarchy | yes, recursive despawn (run) | no | no | no | yes (run) |
| Parallel scheduler | yes, with `multi_threaded` (run: 200 vs 400 ms) | no | yes (run) | no | yes (src) |
| Hooks / observers | yes (run) | no | yes (src) | no | yes (run) |

### 6.4 Scene formats
- **Conditions:** typed structs, 100,000 entities × 3 components, release build, under load; the ratios are what count.

| Format | Size | Serialize | Deserialize |
|---|---|---|---|
| JSON | 21.9 MiB | 69 ms | 93 ms |
| RON | 21.5 MiB | 345 ms | 797 ms |
| TOML | 25.4 MiB | 594 ms | 1622 ms |

- **Diff size** for one changed field plus one added entity, in lines: RON 15, TOML 27, JSON 38. With our scalar-list collapsing, JSON diffs show one line per changed vector.
- **Comments:** RON and TOML have them; JSON does not.
- **Floats:**
  - All three formats emit the shortest round-trip text for f32.
  - Default serde_json f64 parsing was off by one ulp for 7.9% of ordinary values; `float_roundtrip` gave 0.
  - serde_json writes NaN and inf as `null` silently.

### 6.5 bevy_reflect field access (release; under load)
Each iteration is one write plus one read of `translation.x`.

| Access | ns per iteration |
|---|---|
| Typed `get_mut` | 14–16 |
| Reflect string path, component hoisted | 129–135 |
| Cached `ParsedPath` | 57–63 |
| Fully dynamic (name lookup + `reflect_mut` + path) | 169–209 |

That is comparable to our registry (about 107 ns per fully by-name read).

The clean build of bevy_ecs + bevy_reflect took 1m23.8s wall (debug) and 69 crates.

## 7. Gotchas (exact errors)

1. **f32 double rounding through f64 (found by exhaustive test; verified).**
   - Parsing JSON text as f64 and then casting to f32 is wrong for exactly **2 of 4,278,190,080** finite f32 values: `7.038531e-26` and its negative (bits `0x15ae43fd` / `0x95ae43fd`). serde_json writes `7.038531e-26`; reading it as f64 and casting gives `7.0385313e-26`.
   - Fix (`doc::f32_from_parsed_f64`): if the cast is not exact, re-parse the f64's shortest decimal form as f32. Re-running the exhaustive check with the fix gave **0 mismatches across all 4,278,190,080 finite f32 values** (`examples/f32_exhaustive.rs`, 2170 s on 4 threads). RON on a 1-in-7 sample had 0 mismatches.
   - The generic lesson: any value-tree format that parses numbers before knowing the field type needs this. bevy_reflect's typed deserializer would avoid it, and so would a schema-driven `DeserializeSeed`.
2. **ron 0.12.2 `deserialize_any` is quadratic.**
   - `Parser::next_bytes_is_float` (`ron-0.12.2/src/parse.rs:1040`) does `self.src()[skip..].find("..")`, which scans the *entire rest of the input* for a range operator on every number read through `deserialize_any`. Typed structs take a different path and are linear.
   - Measured (`examples/ron_scaling.rs`, `examples/ron_probe.rs`):

     | Entities | JSON | Our RON | `ron::Value` |
     |---|---|---|---|
     | 1,000 | 8 ms | 2.18 s | 2.02 s |
     | 2,000 | 7 ms | 7.95 s | 7.93 s |
     | 4,000 | 14 ms | 32.5 s | 32.2 s |

   - The minimal case: 4,000 small structs parse in 6.8 ms typed but take 755 ms as `ron::Value` or `serde_json::Value`.
   - At 100,000 entities our RON load ran for over 9 minutes before I killed it.
   - The likely fix is bounding the `find` to `raw_float_len`. That is untested, and upstream status is unknown.
3. **bevy_ecs 0.20 renamed the `on_replace` hook to `on_discard`.** The macro constant is `ON_DISCARD = "on_discard"` in `bevy_ecs_macro_logic-0.20.0/src/component.rs:530`. Lifecycle events are now `Add, Insert, Discard, Remove, Despawn`. Most online material is older.
4. **`ComponentDescriptor::new_with_layout` takes 8 arguments in 0.20**: name, storage, layout, drop, mutable, summary_tick, clone_behavior, relationship_accessor. `summary_tick` must be false unless storage is Table (asserted).
5. **`Changed<ChildOf>` does not fire when `ChildOf` is removed.** Incremental transform propagation must also read `RemovedComponents<ChildOf>`; the test covers this.
6. **Entity references dangle across despawn and undo.** An earlier version failed `undo_redo_restores_identical_scenes`: after despawn + undo, `Weld.other` pointed at the old `Entity` (the restored crate is a new `Entity`) and saved as `null`. Despawn now clears inbound references (found with `Registry::references_to`, which walks only archetypes holding entity-typed fields), and its inverse restores them by stable id.
7. **macro_rules ambiguity** with `$(#[doc = $doc:literal])* $(#[$attr:meta])*`:
   ```
   error: local ambiguity when calling macro `component`: multiple parsing options: built-in NTs meta ('attr') or 1 other option.
   ```
   I worked around it with a `requires(GlobalTransform)` clause after the struct name. A proc-macro derive removes the problem.
8. **`Children` iterates `&Entity`:**
   ```
   error[E0277]: the trait bound `Vec<(bevy_ecs::entity::Entity, Affine3A)>: Extend<(&bevy_ecs::entity::Entity, Affine3A)>` is not satisfied
   ```
9. **serde_json pretty-printing puts every vector component on its own line** (5 lines per vec3). `scene::collapse_scalar_lists` is a string-aware post-pass that puts any list without nested lists or maps on one line, for both JSON and RON.
10. **RON writes `Option` as `Some("level")`.** With `Extensions::IMPLICIT_SOME` the file starts with `#![enable(implicit_some)]` and writes `parent: "level"`.
11. **serde_json 1.0.154 formats floats with `zmij`, not `ryu`.** Its default f64 *parsing* is not exact (a formats test found 7.9% of ordinary values off by one ulp); the `float_roundtrip` feature fixes that.
12. **bevy_ecs pitfalls:**
    - `multi_threaded` is not a default feature; without it a Schedule runs serially (two 200 ms systems took 400 ms vs 200 ms).
    - Component names in panics need bevy's `debug` feature, otherwise `<Enable the debug feature to see the name>`.
13. **bevy_reflect 0.20 (for the record):**
    - It depends on glam **0.33.12**. A glam 0.34 field fails with `` error[E0277]: `Vec3` does not implement `GetTypeRegistration` ... note: there are multiple different versions of crate `glam` in the dependency graph ``.
    - `try_apply(&50.0f64)` on an f32 field gives `` `f64` is not `f32` ``. `ReflectComponent::apply` with an f64 panics at `bevy_reflect-0.20.0/src/reflect.rs:212:48`.
    - Its documentation feature was renamed to `reflect_documentation`.
14. **flecs_ecs 0.2.2:** `world.query::<&Hp>().set_cached().build()` then `q.is_changed()` without `.detect_changes()` gives `Segmentation fault` (debug and release); with ECS asserts forced on it gives `Aborted` (exit 134).

## 8. Roblox-style object model on this core

How each piece of the Roblox model maps onto this core:

| Roblox concept | Mapping onto this core | Fit |
|---|---|---|
| Instance tree, `Parent =`, `Destroy()` | entity + `ChildOf`/`Children` (ordered, recursive despawn); `reparent` / `despawn` commands | Direct |
| `Name`, `FindFirstChild(name)` | `Name` component; scan the children list (lists are short), or add a per-parent name index later | Direct |
| `ClassName` + typed properties | a **class layer over components**: a class = a marker + a component set + property aliases, e.g. `Part` = Transform + Primitive + RigidBody, `Position` → `Transform.translation`, `Anchored` → `RigidBody.kind`. The registry already gives typed, documented, defaulted properties by name. | Good; class layer not built |
| Attributes (`Get/SetAttribute`) | an `Attributes` component holding a small name→`Value` map. Needs a map field type. Change signals come from per-key diffing. | Needs a map type |
| Tags / CollectionService | a `Tags` component + an index resource kept by hooks, like `IdIndex` | Direct |
| `Changed`, `GetPropertyChangedSignal` | per-component change ticks (verified) plus per-field diff at signal dispatch | Good |
| `ChildAdded`, `DescendantAdded`, `AncestryChanged` | observers on `ChildOf` insert/discard (hooks verified for StableId; observers verified by ecs-compare) | Good |
| `Heartbeat`, `Stepped` | schedule stages | Direct |
| `WaitForChild`, coroutine yields | the Luau scheduler (not part of this spike) | — |
| Multiple Attachments, Welds per Part | child entities, as Roblox already does; entity-typed fields (`Weld.other`) with despawn/undo repair (verified) | Direct |
| `ModuleScript.Source` | script assets referenced by path, not inline in the scene | Design choice |

Other options as hosts for this model:
- **bevy_ecs:** hosts the model well, because relationships, hooks, observers, change ticks and dynamic components cover the Instance tree, signals and script-defined classes.
- **hecs and sparsey:** would need all of that built from scratch.
- **shipyard:** has hooks and change tracking but no hierarchy.
- **flecs:** has hierarchy, meta-reflection and observers, but brings a C toolchain and the segfault above.

## 9. Stable ids, runtime handles and procedural content

- **Two kinds of identity.**
  - `Entity` (index + generation) is a runtime handle. It is fast, never written down, and differs between runs.
  - `StableId` (a string) is the persistent name that files, commands, logs, undo history, agents and network messages use. The `IdIndex` resource maps one to the other, and hooks keep it exact.
- **Format of ids.**
  - Engine-made ids look like `e7kq2m9x4tb` (50 random bits).
  - Agents and humans may choose readable ones (`player_spawn`); uniqueness is checked.
  - `/` is reserved for generated ids.
- **Entity-typed fields.** They hold `Entity` at runtime and are written as ids. Despawn repairs inbound references so undo is exact.
- **Generated content.**
  - Each output node has a generator-chosen key. Its id is `<source id>/<key>`, so editor selection, agent commands and overrides address generated things like any other entity.
  - Determinism is per key (`ctx.random(key)` hashes seed + key). Growing a grid does not reshuffle existing pillars, so overrides stay meaningful.
- **Authored vs generated.** The file stores the source (`Procedural`, the parameter component, its own transform) plus `overrides`: a list of (target key, component, field, value). Generated entities are excluded from saves, and structural edits on them are refused with a hint.
- **Regeneration.** Each `update()` fingerprints the generator name, seed and parameter values. If they changed, it reruns the generator, despawns the old output, spawns the new, and re-applies overrides by key. Overrides whose key disappeared are kept and reported. A failing generator leaves the previous output in place.
- **Generators return plain data.** A generator returns `GenNode { key, name, parent key, components by name }`, so a Luau generator can produce exactly the same thing.
- **Not built yet:**
  - **Bake:** turn output into authored entities.
  - **Structural overrides:** deleting or adding under generated nodes.
  - **Prefabs / scene instancing:** these fit the same source + overrides model.
  - **Generator versioning.**
  - **Async or threaded generation.**

## 10. Windows and macOS (unverified)

- Nothing in `re_core` is platform-specific. bevy_ecs 0.20 with `multi_threaded` uses std threads through bevy_tasks, and the rest is pure Rust (serde, glam, indexmap). Both should build on Windows (MSVC) and macOS (aarch64/x86_64). **Unverified:** no non-Linux build was run.
- The `IdGen` seed uses `SystemTime` and the process id, which exist on both.
- If flecs had been chosen, Windows would have needed clang for bindgen; that is another reason it was dropped.

## 11. Open questions

1. **Comments in scene files.**
   - JSON has none. Options:
     - a per-entity `"note"` field;
     - accepting JSONC/JSON5 on load while writing plain JSON;
     - switching to RON once its parser is fixed.
   - Decide before the editor writes scenes users hand-edit.
2. **Container field types.**
   - Attributes, tag lists and spline points need list and map fields.
   - Adding `List(FieldType)` and `Map` to the closed type set is straightforward, but it affects the schema, Luau bindings and diffing.
3. **Derive macro.** Replace `component!` with `#[derive(Component, Reflect)]`-style proc macros in an `re_macros` crate. That gives better errors, arbitrary attributes, and nested structs flattened to field paths.
4. **Undo coalescing.** Merge consecutive `set_field`s on the same field during a drag into one undo step, using a gesture id on the transaction.
5. **Transaction log persistence and agent sessions.**
   - Today the log is in memory, with `log_since_jsonl(seq)` for polling.
   - The AI-integration layer should decide how to persist it, how agents subscribe, and how to scope permissions per origin.
6. **Spawn throughput through commands.** A Spawn carries untyped `Doc` maps that are converted per field. If bulk spawns matter, add a typed fast path (e.g. `spawn_batch`) beside commands for runtime gameplay; keep commands for editing.
7. **Threading model with Luau.**
   - All mutation goes through `&mut Core`.
   - Script calls run on the main thread between schedule runs; parallel systems stay typed Rust.
   - This needs confirming with the scripting spike.
8. **bevy_ecs upgrade policy.** Pin `=0.20.0` and upgrade deliberately. The engine API (`Core`, `Registry`, `Command`, `Doc`) must not leak bevy types beyond `Entity` and `World` access, so each bevy release touches one crate.
9. **Should `GlobalTransform` and other derived data be readable by name** (read-only fields) for agents and the inspector? This is probably yes, through a "derived" flag on fields.

## 12. Files

The prototype was ported into three crates (M1 WP0, PR "entity core"):

| Spike file | In this repository |
|---|---|
| `src/value.rs`, `registry.rs`, `doc.rs`, `schema.rs` and the registry part of `error.rs` | `crates/re_reflect/src/` |
| `src/ids.rs`, `components.rs`, `hierarchy.rs`, `scene.rs`, `procedural.rs`, `core.rs` (the `World` facade) | `crates/re_scene/src/` |
| `src/commands.rs` | `crates/re_command/src/` (`op.rs`, `history.rs`) |
| `tests/core.rs` (15 tests) | `crates/re_reflect/tests/reflect.rs`, `crates/re_scene/tests/scene.rs` and `crates/re_command/tests/command.rs` |
| `examples/bench.rs` (the 100k benchmark) | `crates/re_scene/examples/core_bench.rs` |
| `examples/demo.rs` (what an agent sees) | `crates/re_command/examples/agent_demo.rs` |
| `examples/f32_exhaustive.rs` (all 2^32 f32 values) | `crates/re_reflect/examples/f32_exhaustive.rs` |

Not in this repository: the spike's `examples/ron_scaling.rs` and `ron_probe.rs` (the RON quadratic), its demo output files, and the raw material from the three comparison runs (formats, ECS and reflection; logs, pasted outputs and the throwaway comparison crates), which sections 3, 6 and 7 summarise.
