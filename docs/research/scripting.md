# Scripting spike: Luau in Real Engine (mlua 0.12.2, Luau 0.740)

Status: prototype built, compiled and tested on 2026-10-14. All 14 feature tests pass in three builds: the default build, `--features send` and `--features jit`.

This is the scripting spike's report, published as M1 WP3 asks. It is historical: the prototype was a standalone Cargo project in the private research bundle, and WP3 ported it into [`crates/re_script/`](../../crates/re_script/) and [`crates/re_script_check/`](../../crates/re_script_check/). Paths such as `code/src/vm.rs` and `code/tests/features.rs` below refer to that prototype, not to this repository.

**Where things were**
- Prototype source: `code/` (no build output).
- Raw test output and benchmark runs: `code/results/`.
- Analyzer work (patch, definitions, test projects): `code/analyzer/`.

**Environment.** A Linux container with 4 CPUs, no GPU and **rustc/cargo 1.97.0** (this container has 1.97, not the 1.99 listed in environment.md). Everything was built against crates.io through the proxy.

## 1. Decision summary

- **Use `mlua =0.12.2` with the `luau` feature only.**
  - Don't enable `vendored`: it does nothing for Luau, because Luau is always built from `luau0-src`.
  - Don't enable `send` by default. The `jit` feature is worth offering as an opt-in.
- **Sandboxing.** Run untrusted scripts in one sandboxed VM (`Lua::sandbox(true)`). Give each script its own environment table, with `safeenv` set on it. Remove `loadstring`, `getfenv` and `setfenv`, and replace `require`.
- **Time budget.** Enforce it with a raw `lua_Callbacks.interrupt` (`src/watchdog.rs`), not `Lua::set_interrupt`. The raw version is about 2× cheaper per loop iteration, and once tripped it cannot be swallowed by `pcall`.
- **Entity handles.** Script-visible entities are `Copy` handles (index + generation) implemented as userdata, interned per entity, that look up a `World`. Scripts never own engine data.
- **Waiting.** `task.wait` yields from a Luau stub. A Rust scheduler owns suspended threads and resumes them from the game loop.
- **Types and diagnostics.** These come from outside the VM:
  - a generated `engine.d.luau`;
  - either the official `luau-analyze` 0.740 with a 27-line `--definitions` patch, or luau-lsp 1.70.1, which works unpatched;
  - and a shared `.luaurc` so the checker and the runtime resolver agree on aliases.
- **Threading.** Keep each VM owned by one thread: the main thread, or a script phase on the main thread. Use more VMs, one per isolated domain, for parallelism rather than `send`.

## 2. Crates

| Crate | Version | License | MSRV | Role |
|---|---|---|---|---|
| mlua | =0.12.2 | MIT | 1.88 | high-level bindings |
| mlua-sys | 0.13.0 | MIT | 1.88 | FFI and the build of Luau |
| luau0-src | 0.22.0+luau740 | MIT (Luau © Roblox) | none | vendored Luau 0.740 C++ sources: Ast, Bytecode, CodeGen, Common, Compiler, Config, Custom, Inliner, Require, VM. **No Analysis.** |
| glam | =0.34.1 | MIT OR Apache-2.0 | 1.85 | `Vec3` |

- **Transitive dependencies:** bstr, either, libc, num-traits, parking_lot, rustc-hash and cc. All are MIT or Apache-2.0, except memchr, which is Unlicense OR MIT. The full list came from `cargo metadata`.
- **Prototype MSRV:** 1.88, because of mlua. The `let`-chains in `diag.rs` need edition 2024, which needs 1.88 or later.
- **External tools,** not linked into the engine:
  - Luau CLI 0.740 release, MIT: `luau`, `luau-analyze`, `luau-compile`, `luau-ast` (from https://github.com/luau-lang/luau/releases/download/0.740/luau-ubuntu.zip).
  - luau-lsp 1.70.1, MIT (https://github.com/JohnnyMorganz/luau-lsp/releases/download/1.70.1/luau-lsp-linux-x86_64.zip). Its Luau submodule is pinned to the same commit as 0.740 (`c0e346e`).

**Rejected options**
- **Lua 5.4 / LuaJIT through mlua:** the brief says Luau, and Luau adds the sandbox, the native `vector` type, gradual types and an interrupt designed for embedding.
- **Raw `mlua-sys` or hand-written FFI:** mlua's userdata, conversions and error plumbing are solid. The prototype drops to FFI in exactly one place, the interrupt, where it pays off.
- **The `luau-analyze` 0.0.1 and `ruau` 0.4.0 crates:** they have about 360 downloads each and are unproven. They also either re-implement the type checker or would need Analysis built into the engine binary. Running the official tools out of process is safer and keeps the engine binary small.
- **mlua's `async` feature for waiting:** `Lua::yield_with` exists only as an `async fn`. A synchronous Luau stub that yields is simpler and needs no executor in the game loop.
- **mlua's default `FsRequirer`:** it resolves against the process working directory and accepts absolute paths. That is fine for tools, but not for untrusted scripts.

## 3. Verified facts (all from code that ran)

The test names are in `code/tests/features.rs`, and the exact output is in `code/results/features_output.txt`.

### Sandbox (`sandbox_blocks_dangerous_globals_and_freezes_libraries`)
- Defaults: `Lua::new()` (StdLib::ALL_SAFE, which under Luau means every Luau library) installs `loadstring` and a filesystem `require`. The engine sets `loadstring`, `getfenv` and `setfenv` to nil *before* `sandbox(true)`, because globals are read-only afterwards. `os.execute` and `io` don't exist in Luau.
- After `sandbox(true)`, `math.sqrt = ...`, `string.rep = nil` and `getmetatable('').__index = {}` all fail with `attempt to modify a readonly table`. The vector metatable is also read-only (test `vector_metatable_under_sandbox` reports `writable=false`).
- **Script isolation.** Each script or module gets `env = setmetatable({}, {__index = globals})`, with a read-only metatable and `env.set_safeenv(true)`; this is `vm::new_env`. A global written by script A is nil in script B.
- **Why `safeenv` matters.** It keeps builtin fastcalls and import caching on. In a `math.sqrt` loop it measured **22–23 ns/iter with safeenv and 89–92 ns without**, a 4× difference.

### Entity userdata (`entity_userdata_fields_methods_metamethods`)
- The `Entity(EntityId{index, generation})` userdata has:
  - fields: `id` (get only), `alive`, `name` (get and set), `position` (get and set, as a vector);
  - methods: `translate`, `distance`, `get_tag`, `set_tag`;
  - metamethods: `__eq` and `__tostring`.
- The `World` lives in `lua.set_app_data`.
- **Handle interning.** Handles are interned in a weak-valued registry table (`entity::handle`). As a result `world.query{tag="enemy"}[1] == b` and `rawequal(...)` are both true, and handles work as table keys.
- **Error messages scripts see:**
  - wrong type for a field: ``bad argument #2 to `Entity.position`: error converting Lua integer to vector (expected a vector, e.g. vector.create(x, y, z))``
  - stale handle after despawn: `Entity(3v0) is no longer alive (stale handle)`
  - unknown method: `unknown:1: attempt to call missing method 'jump' of Entity`
  - writing a get-only field: `attempt to set an unknown field 'id'`. This message is misleading; see Gotchas.

### Vector ↔ glam (`vector_maps_to_glam`)
- `Value::Vector(mlua::Vector)` is 3 × f32 by default, and `typeof(v) == "vector"`.
- `V3(pub Vec3)` implements `FromLua` and `IntoLua`; a newtype is needed because of the orphan rule.
- `v * 2 + vector.create(0, 0, vector.magnitude(v))` round-trips to `Vec3(6, 8, 5)`. Passing a vector from Rust and getting `vector.normalize` back works.
- Passing `{1,2,3}` where a vector is expected gives `error converting Lua table to vector`.

### Rust functions with tables and closures (`rust_functions_with_tables_and_closures`)
- `world.query({near=, radius=, max=})` reads an options table and returns an array table of handles.
- `world.spawn_grid(n, function(i) return vector end)` calls a Luau closure from Rust once per entity.
- `world.counter(10)` returns a stateful Rust closure, made with `create_function_mut`; the test result was `12, 17`.
- An `error()` thrown inside the Luau closure surfaces as `cb_err:1: placement failed`.

### Events and scheduling (`rust_calls_event_handlers_which_can_wait`, `coroutine_scheduler_wait_defer_delay`)
- `events.on(name, fn)` stores handlers in a registry table. Rust's `vm.fire("damage", (entity, 7))` spawns each handler as its own thread on the next `tick`, which is deferred semantics.
- Handlers can `task.wait`. One failing handler (`scripts/events.luau:7: handler two broke`) does not stop the others.
- **Mechanism.** `task.wait(dt)` is a 6-line Luau stub, `coroutine.yield(SENTINEL, dt)`. `scheduler::step(now)` resumes due threads and passes the actual elapsed time back as `task.wait`'s return value. `task.defer` and `task.delay` queue new threads.
- With 0.1 s ticks, `task.wait(0.25)` returned `0.30`, as expected from the tick granularity.
- **`pcall` across a yield works:** `pcall(function() task.wait(.25) error("inside pcall") end)` returns `false, scripts/loop.luau:8: inside pcall`.
- **Yielding across a Rust call is impossible.** Calling `task.wait` inside a callback that Rust invoked (such as the `spawn_grid` closure), or from `exec`, gives the stub's own message: `task.wait can only be called from a script thread, not from a callback`. That message comes from checking `coroutine.isyieldable()` first.

### `require` (`require_resolves_inside_project_only`)
- `ProjectRequirer` (`src/modules.rs`) implements `mlua::luau::Require` over a virtual path inside the project root. It supports:
  - `./x` and `../x`;
  - `@game/...`, an engine alias override for the root;
  - `@self/x` from an `init.luau`;
  - aliases from the project `.luaurc`; `@shared` was tested.
- `scripts/main.luau` loads one module through three different paths and receives the same table each time.
- **Path escape:** `require("../../../../etc/passwd")` is rejected by Luau's own navigator, with `error requiring module "../../../../etc/passwd": could not get parent of component ".."`, because `to_parent` refuses to go above the root.
- **Chunks not loaded from a file:** chunks named `=...` cannot `require` at all; the error is `require is not supported in this context`.
- **Cache:** mlua caches results in the registry table `__MLUA_LOADER_CACHE`, keyed by our `cache_key` (`lib/util.luau`, `lib/inventory/init.luau`, …).

### Errors (`errors_have_chunk_line_and_traceback`)
- `ScriptError {kind, context, chunk, line, message, traceback}` is built from mlua errors.
- **Runtime error in a script thread:** `[Runtime] scripts/broken.luau:2: attempt to index nil with 'field'`, with traceback `scripts/broken.luau:2 function inner / :5 function outer / :7`. The traceback comes from `debug.traceback(thread)` on the dead coroutine; its stack survives the error.
- **Syntax errors:** `[Syntax] scripts/syntax.luau:1: Expected identifier when parsing expression, got '='`.

### Hot reload (`hot_reload_keeps_state_and_swaps_functions`)
- **Convention:** on reload, the chunk is called with its current module table as `...`. A module written as `local M = ... or {}; M.count = M.count or 0` is patched in place, so every holder of `M` sees the new functions and the old state.
- **Test sequence:**
  1. v1 runs and prints `v1 1`, `v1 2`.
  2. The file is edited; `reload_changed()` detects the mtime change and reports `InPlace`.
  3. Output continues as `reloaded with count 2`, `v2 12`.
- A reload with a syntax error is reported and the running version keeps going (`v2 22`).
- A module that ignores `...` gets `Replaced`: later `require` calls see the new table, while existing holders keep running v2 (`v2 32`).
- **The naive approach failed when measured.** The first approach copied new functions onto the old table. It printed `v2 1010`, because the new functions close over the fresh chunk's own local `M`.

### Time budget (`interrupt_stops_runaway_loops`, `examples/jit_interrupt.rs`)
- `while true do end` with a 20 ms budget stopped after **20.2 ms** with `[Timeout] script exceeded its time budget`.
- A spinning script thread dies while another script in the same tick still runs (`still alive`).
- **`pcall` cannot swallow the timeout.**
  - The first version allowed `for i=1,1e9 do pcall(function() while true do end end) end` to run effectively forever: each `pcall` caught the error and the outer loop continued.
  - Fix: once tripped, every safepoint raises until the budget is re-armed, so the outer loop's back-edge raises outside the `pcall`.
- **Native code:** a loop inside a function stopped after 50.2 ms in both the interpreter and the `jit` build. That shows the interrupt fires in native code too.

### Memory limit (`memory_limit_raises_and_vm_recovers`)
- `set_memory_limit(8 MiB)`: a filling loop fails with `MemoryError("not enough memory")`, which maps to `ErrorKind::Memory`.
- After `gc_collect()` the same VM runs new code fine.
- `pcall(string.rep('x', 64 MiB))` returns `false, "not enough memory"`. Scripts *can* catch out-of-memory errors.
- The baseline VM with the engine API uses **504,072 bytes**.
- **Coroutine cost:** a VM with a 64 MiB limit ran out of memory when 100,000 handler threads were pending at once (the events benchmark at 100k). Budget roughly 0.5–0.6 KB per suspended thread, plus its arguments. This is an inference from that failure, not a precise measurement.

### Threading (`tests/send_sync.rs`, `examples/not_send.rs.txt`)
- **Without `send`:** moving a `Lua` into `std::thread::spawn` fails to compile with ``error[E0277]: `Rc<mlua::types::sync::inner::ReentrantMutex<mlua::state::RawLua>>` cannot be sent between threads safely``.
- **With `--features send`:**
  - `Lua`, `Table`, `Function` and `ScriptVm` are `Send`, and `Lua` is `Sync`.
  - A `ScriptVm` moved to another thread ran `return 40 + 2` there.
  - All 14 tests pass.
- **Consequence for `Require` and other callbacks.** These implementations must be `MaybeSend`, so without `send` they cannot capture `Table` or `Function` values; the compile error is `Rc<…> cannot be sent`. The prototype stores the globals table in the registry instead (`vm::GLOBALS_KEY`).

### Corpus (`src/bin/corpus.rs`)
- An external Roblox-style corpus compiled with no failures.

## 4. Type definitions and diagnostics (verified)

The vendored Luau cannot type-check in process, so diagnostics come from the official tools.

- **Definitions file syntax for 0.740.** Use `declare extern type Entity with … end` for userdata: fields marked `read` are read-only, methods take `self`, and metamethods are written `function __eq(self, other: Entity): boolean`. Library tables are declared with `declare world: { … }`.
  - **`declare class` is rejected** in 0.740, with `SyntaxError: Expected ':' when parsing global variable declaration, got 'Entity'`.
  - The prototype's real API is described in `code/types/engine.d.luau`.
- **Stock `luau-analyze` 0.740 has no flag to load definitions;** it ends with `Unknown global 'world'`.
  - The patch `code/analyzer/luau-analyze-definitions.patch` adds `--definitions=PATH` (27 lines).
  - It built from tag 0.740 in **220 s** with `-j4`. The stripped binary is 5.3 MB.
- **luau-lsp 1.70.1** works unpatched:
  `luau-lsp analyze --platform=standard --flag:LuauSolverV2=true --definitions=@engine=engine.d.luau files…`
  - It defaults to the old solver, which misses writes to `read` fields. Always pass `--flag:LuauSolverV2=true`.
  - The LSP over stdio initialised in 0.05 s and published diagnostics in 0.10 s.
- **Checked against the prototype's own scripts:**
  - `scripts/typed.luau` is clean, apart from one unused-local lint.
  - `scripts/typed_bad.luau` gives, with exit code 1:
    ```
    ./scripts/typed_bad.luau:3.34-3.34: TypeError: Expected this to be 'vector?', but got 'number'
    ./scripts/typed_bad.luau:4.1-4.9: TypeError: Key 'jump' not found in external type 'Entity'
    ./scripts/typed_bad.luau:5.1-5.7: TypeError: Property id of type 'Entity' is read-only
    ./scripts/typed_bad.luau:6.21-6.40: TypeError: Expected this to be 'string', but got 'string?';
    `nil` is not a subtype of `string`
    ```
- **Shared `.luaurc`.** The same file drives both the analyzer and `ProjectRequirer`: `{"aliases": {"shared": "./lib", "game": "./"}}`.
  - `"game": "."` is **not** accepted by the analyzer (`Unknown require: unsupported path`), while `"./"` is.
  - At runtime `@game` is also an engine override, so both sides agree.
- **Output formats.** `--formatter=gnu` writes `file:l.c-l.c: Kind: msg` to stderr. Lines that don't start a new diagnostic continue the previous message. Exit codes: 0 means clean (lint warnings alone still give 0), 1 means errors, 2 means an internal compiler error. There is no JSON output. `--formatter=plain` always exits 0. For structured data, use the LSP's `publishDiagnostics`.
- **Recommendation:**
  - Generate `engine.d.luau` from the Rust binding registry, so every `add_method` and `add_field` has a type signature next to it, and check the generated file in CI by loading it.
  - Ship luau-lsp for editors and AI agents.
  - Ship either luau-lsp `analyze` or the patched `luau-analyze` for `engine check`.
  - Parse gnu-format output into the same `ScriptError` shape the runtime uses, so editor, CLI and agent tooling all see one diagnostics format.

## 5. Benchmarks

`cargo run --release --bin bench`, best of 5 runs, ns/op. The two default runs show the noise level; the raw files are in `code/results/`.

| Operation | default (run 1 / run 2) | `send` | `jit` |
|---|---|---|---|
| Rust calls Luau fn (2 in, 1 out) | 122 / 128 | 154 | 126 |
| Luau calls Rust closure | 62 / 65 | 61 | 56 |
| `entity:translate(vector)` (userdata method + World lookup) | 246 / 272 | 345 | 241 |
| `entity.position` get | 240 / 232 | 258 | 236 |
| native vector `p = p + v * 0.5` | 19 / 19 | 28 | 15 |
| table `{x,y,z}` version (allocates) | 271 / 139 | 142 | 82 |
| fib(30), per call | 81 / 44 | 44 | 27 |
| `math.sqrt` loop, engine VM (safeenv, raw interrupt) | 23 / 22 | 22 | 6 |
| same, env without safeenv | 92 / 89 | 92 | 95 |
| same, plain sandboxed VM, no interrupt | 18 / 18 | 19 | 3 |
| loop iteration, raw interrupt | 13.5 / 13.2 | 12.8 | 5.8 |
| loop iteration, `Lua::set_interrupt` | 28.4 / 28.5 | 43.1 | 20.2 |
| loop iteration, no interrupt | 9.7 / 10.5 | 10.1 | 3.1 |
| scheduler: resume + `task.wait`, 10k threads | 1,263 / 1,112 | 1,273 | 2,116 |
| event fire + handler thread run | 3,541 / 2,947 | 3,336 | 3,400 |
| compile + load source | 8–10 MB/s | 11 MB/s | 2.5 MB/s |

What this shows:
- **Interrupt cost.** The raw interrupt costs about 3–4 ns per safepoint. `Lua::set_interrupt` costs about 18 ns, and about 33 ns with `send`, where the reentrant mutex is taken on every callback.
- **The `send` feature** costs 25–40% on Rust↔Luau and userdata calls.
- **The `jit` feature:**
  - It gives 2–6× on numeric loops.
  - It is 4× slower to load source, because of native compilation.
  - Its release rebuild took 2 min 10 s, against 1 min 48 s for a clean default release build.
  - Scheduler resumes measured 2× slower under JIT in one run. This has not been investigated.
- **Userdata methods** at about 250 ns are the slowest path. The cost is mlua's `__index` dispatch plus two `RefCell`-style borrows (the userdata and the `World` app data). Hot engine APIs should therefore be batch-shaped (for example `world.query`) rather than per-component calls. The next experiment is `UserDataRef`-free handle methods taking `AnyUserData`, or a `__namecall`-based fast path.
- **Scheduler at about 1.1 µs per resume** is acceptable for thousands of script threads per frame, but not for 100k. The prototype rebuilds the pending `Vec` with `partition` on every pass; a binary heap keyed by wake time is the obvious fix.

## 6. Build times (this container, 4 cores)

| Build | Time |
|---|---|
| Clean debug | 47 s (mostly Luau C++) |
| Clean release | 1 min 48 s |
| Release rebuild with `--features jit` (mlua-sys rebuilt with CodeGen) | 2 min 10 s |
| Incremental debug after touching `vm.rs` | 0.9 s |
| `luau-analyze` from source (cmake, RelWithDebInfo, analyzer target only) | 220 s |

The Luau C++ build is a one-time cost per profile. Setting `[profile.dev.package.mlua-sys] opt-level = 3`, so that debug builds get a fast interpreter, has not been tried yet.

## 7. API cheat-sheet (excerpts from the compiling prototype)

```rust
// VM setup (src/vm.rs)
let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
let g = lua.globals();
g.raw_set("loadstring", Value::Nil)?;           // also getfenv / setfenv
g.raw_set("world", create_world_lib(&lua)?)?;
g.raw_set("require", lua.create_require_function(ProjectRequirer::new(root, loaded))?)?;
lua.sandbox(true)?;                              // freezes everything above
lua.set_memory_limit(64 << 20)?;
watchdog::install(&lua);                         // raw interrupt, see src/watchdog.rs

// Per-script environment with fastcalls kept on
let env = lua.create_table()?;
let mt = lua.create_table()?; mt.raw_set("__index", &globals)?; mt.set_readonly(true);
env.set_metatable(Some(mt))?; env.set_safeenv(true);
lua.load(src).set_name("@scripts/main.luau").set_environment(env).into_function()?;

// Userdata handle (src/entity.rs)
impl UserData for Entity {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("position", |lua, this| with_world(lua, |w| Ok(V3(w.get(this.0)?.position))));
        f.add_field_method_set("position", |lua, this, p: V3| with_world(lua, |w| { w.get_mut(this.0)?.position = p.0; Ok(()) }));
    }
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("translate", |lua, this, d: V3| with_world(lua, |w| { w.get_mut(this.0)?.position += d.0; Ok(()) }));
        m.add_meta_method(MetaMethod::Eq, |_, this, other: mlua::UserDataRef<Entity>| Ok(this.0 == other.0));
        m.add_meta_method(MetaMethod::ToString, |lua, this, ()| /* ... */);
    }
}

// Vector bridge (src/math.rs)
impl FromLua for V3 { fn from_lua(v: Value, _: &Lua) -> Result<Self> {
    match v { Value::Vector(v) => Ok(V3(Vec3::new(v.x(), v.y(), v.z()))), /* error */ } } }
impl IntoLua for V3 { fn into_lua(self, _: &Lua) -> Result<Value> {
    Ok(Value::Vector(Vector::new(self.0.x, self.0.y, self.0.z))) } }

// Closures both ways
lua.create_function(|lua, (n, place): (u32, Function)| { let p: V3 = place.call(1)?; /* ... */ })?;
lua.create_function(|lua, start: i64| { let mut n = start;
    lua.create_function_mut(move |_, step: Option<i64>| { n += step.unwrap_or(1); Ok(n) }) })?;

// Scheduler resume (src/scheduler.rs)
match task.thread.resume::<MultiValue>(args) {
    Ok(y) if task.thread.status() == ThreadStatus::Resumable => /* (SENTINEL, dt) => sleep */,
    Ok(_) => /* finished */, Err(e) => ScriptError::from_thread(lua, &task.thread, &label, e),
}

// Raw interrupt (src/watchdog.rs)
unsafe extern "C-unwind" fn interrupt(L: *mut ffi::lua_State, gc: c_int) {
    if gc >= 0 { return; }
    /* every 1024 safepoints: if Instant::now() > deadline { tripped = true } */
    if tripped { unsafe { ffi::luaL_error(L, c"script exceeded its time budget".as_ptr()) }; }
}
lua.exec_raw::<()>((), |L| (*ffi::lua_callbacks(L)).interrupt = Some(interrupt))?;
```

```luau
-- Script side
local hero = world.spawn("hero", vector.create(0, 1, 0))
hero:translate(vector.create(1, 0, 0))
for _, e in world.query({ near = hero.position, radius = 5 }) do print(e.name) end
events.on("damage", function(target, amount) task.wait(0.5) print("recovered", target.name) end)
local M = ... or {}            -- hot-reloadable module convention
M.count = M.count or 0
return M
```

## 8. Gotchas (with the exact errors)

1. **`MaybeSend` callbacks cannot capture Lua values without `send`.** The error is ``E0277: `Rc<ReentrantMutex<RawLua>>` cannot be sent between threads safely``. Keep Lua values in the registry (`set_named_registry_value`) or in Lua-side tables instead.
2. **Several types live in modules, not the crate root.** `mlua::Require`, `NavigateError`, `ThreadStatus` and `Compiler` are not re-exported at the root. Use `mlua::luau::{Require, NavigateError}`, `mlua::thread::ThreadStatus` and `mlua::chunk::Compiler`; otherwise you get `error[E0432]: unresolved imports`.
3. **`add_meta_method(MetaMethod::ToString, |lua, this| …)` fails** with `E0593: closure is expected to take 3 arguments`. The closure must be written `|lua, this, ()|`.
4. **`init.luau` stands for its folder.** `@self/x` failed with `no module present at resolved path` until `reset` mapped `…/init.luau` to its folder. Note that Luau's `@self` handling does not call `to_parent`.
5. **`pcall` swallows `Lua::set_interrupt`-style timeouts** unless the interrupt keeps raising once tripped (see §3). Both interrupt implementations now keep raising.
6. **Copying new functions onto an old module table is not hot reload.** The new functions close over the new chunk's locals (`v2 1010`). Use the `...` convention.
7. **A get-only field set from a script** reports `attempt to set an unknown field 'id'`, not "read-only". The engine should wrap this message, or register a setter that raises a clear error.
8. **The `ScriptError` for some errors carries no chunk or line.** This happens for Rust-side errors (bad argument, stale handle) and for timeouts raised at the top level of a chunk. The location is in the traceback (`stale:1: in function <stale:1>`) but not in the first line. The engine should take the first Luau frame from the traceback.
9. **Coroutine tracebacks are short.** `debug.traceback(thread)` on a dead thread lists frames as `scripts/events.luau:7` without the `in function` text; compare `lua.traceback` style.
10. **`luau-analyze` alias values.** `"game": "."` is rejected (`Unknown require: unsupported path`); write `"./"`.
11. **Never kill test processes with `pkill -f <pattern>`.** The pattern can match your own shell and kill it; it happened twice during this spike. Also, test output piped through `grep | head` is lost when a hanging test is killed by `timeout`.
12. **`StdLib::ALL_SAFE` includes `debug` under Luau.** Luau's `debug` library is already reduced (`info`, `traceback`). The engine keeps it; it is how coroutine tracebacks are obtained.

## 9. Multithreaded engine recommendation

- **Default: one script VM per world, owned by the main thread.** Scripts run in a dedicated "script phase" of the frame:
  1. Rust arms the time budget.
  2. It calls `vm.tick(now)` and flushes queued events into the VM.
  3. Scripts mutate the world only through handles; each call borrows the `World` briefly and never across a call back into Lua.
- **Parallel engine work stays out of the VM.** That means rendering prep, physics and asset jobs. Results come back as events on the next script phase.
- **Parallel scripting means more VMs, not a shared VM.** For example, per-actor or per-server-shard VMs, each `!Send` and pinned to its worker thread, exchanging serialised messages. Without `send`, the compiler enforces that pinning.
  - `send` would allow migrating a VM between threads, at a 25–40% call overhead. Every call then takes a lock, and that lock is never contended in this design.
  - Enable `send` only if the job system must move VMs between workers.
- **The raw interrupt's thread-local deadline** matches the one-thread-at-a-time model: `arm` runs on the thread that runs the VM, and this was verified when the `send` build ran a VM on a spawned thread.
- **Luau has no shared-memory parallelism inside one VM.** Roblox-style parallel Luau uses separate VMs (Actors) too.

## 10. Windows and macOS (unverified; nothing was run on them)

- mlua's CI builds `luau`, `luau-jit` and `luau-vector4` on windows-latest (MSVC) and macos-latest (aarch64). luau0-src's CI also covers x86_64-pc-windows-msvc and aarch64-apple-darwin. This comes from reading their CI files, not from a run.
- `extern "C-unwind"` plus `luaL_error` in `watchdog.rs` relies on Luau raising errors with C++ exceptions or longjmp in a way that unwinds through Rust frames. On Linux this works. On MSVC it needs checking, especially for the `-C panic=abort` profile.
- Luau codegen (`jit`) requires x64 with AVX1, or arm64. Apple Silicon is arm64, so it should work there, but this has not been tested.
- Luau release binaries are published for ubuntu, macos and windows; luau-lsp ships per-platform zips. Only the Linux ones were tried.

## 11. Open questions

1. Should script handles be ECS entity IDs directly, or this generation-checked slot handle? That depends on the ECS choice in the architecture thread.
2. How does the engine produce `engine.d.luau`: a proc-macro next to each binding, or a hand-maintained registry checked by a test that loads it in luau-analyze?
3. Which checker ships: the patched `luau-analyze`, which needs a rebase per Luau bump, or luau-lsp only? Both were verified.
4. Should `task.wait` work inside user coroutines (`coroutine.wrap`)? Today such a yield goes to the user's coroutine, not the scheduler; this is a known limitation and was not tested.
5. Should the scheduler move to a heap, and should `fire_event` reuse threads, given the 1.1 µs resume and 3 µs event costs?
6. How should timeouts be attributed? Store the line of the first Luau frame in `ScriptError` (see Gotcha 8).
7. Should the JIT be on by default for release builds (2–6× faster numeric code), and only for modules marked `--!native`?
8. Is an mtime poll enough for hot reload, or should it use a file watcher (the `notify` crate, not evaluated)? Atomic-save editors may change file identity.

## 12. Rerunning

```
cd code
cargo test -- --nocapture --test-threads=1          # 14 feature tests
cargo test --features send                           # Send/Sync proof + same tests
cargo test --features jit
cargo run --release --bin bench
cargo run --release --bin corpus -- <dir-with-.luau-files>...
cargo run --release --example jit_interrupt [--features jit]
```

- **Build location:** build outside the shared folder (copy it, or set `CARGO_TARGET_DIR`).
- **Analyzer checks:** run `analyzer/build/build.sh` after cloning Luau 0.740 into `build/luau-patched` and applying `analyzer/luau-analyze-definitions.patch`. Then:
  `luau-analyze --formatter=gnu --definitions=types/engine.d.luau tests/project/scripts/typed_bad.luau`
  (run from `tests/project`).
