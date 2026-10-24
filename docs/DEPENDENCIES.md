# Dependencies

This file lists every third-party crate Real Engine plans to use in M1: its exact version, license, MSRV, what it is for, how we checked it, and which feature flags we turn on.

## Policy

1. **Only crates.io.** No git dependencies, no path dependencies outside the workspace, and no build scripts that download things. This is why `oxijolt` is used with `default-features = false`: its default `prebuilt` feature downloads binaries at build time.
2. **Adding a dependency needs a DECISIONS entry.** A PR that adds a crate to any `Cargo.toml` must add or amend an ADR in [DECISIONS.md](DECISIONS.md) and a row in this file. Upgrading within the same minor version only needs a row update.
3. **Exact versions.** The workspace sets every dependency in `[workspace.dependencies]` as `"=X.Y.Z"`. Member crates write `foo.workspace = true`. `Cargo.lock` is committed. CI builds with `--locked`.
4. **License allow-list**, enforced by `cargo deny check licenses`: MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, BSL-1.0, ISC, Zlib, CC0-1.0, Unicode-3.0. (BSL-1.0 is for `clipboard-win` and `error-code`, which egui-winit pulls on Windows.) Per-crate exceptions, and nothing else (ADR-0019):
   - MPL-2.0: `full_moon` and `full_moon_derive` (tooling only) and `triple_buffer` (an unconditional dependency of kira). **`triple_buffer` is the only MPL-2.0 crate that ships in the runtime.** kira is built without its `wav` feature, so the symphonia crates are not in the build; `re_audio` decodes WAV itself.
   - OFL-1.1 and Ubuntu-font-1.0: `epaint_default_fonts` (egui's bundled fonts). The compat UI's own font files are OFL-1.1 too; they are bundled assets, not crates (see below and ADR-0035).
   MPL-2.0 is file-level copyleft. We use these crates unmodified, so it puts no obligation on our own code. This list comes from a license scan of a lockfile with every crate and feature below.
5. **Duplicate versions are tracked.** `deny.toml` limits `[graph] targets` to the three desktop triples (`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`) and sets `[bans] multiple-versions = "warn"`, so cargo-deny reports duplicates but the gate is `cargo xtask dupes`. That command runs `cargo tree -d --workspace -e normal --target <triple>` for each of the three triples (it needs no cross toolchain; it only reads the lockfile) and compares the result with the allow-list in `deny.toml` (`[bans] skip`) as **(name, version) pairs**. The same version appearing twice (a host build and a target build with different features) is not a duplicate. A new duplicate fails CI until someone adds it to the list below with a reason.

## Status meanings

- **Verified**: built and exercised by a spike, the render prototype (now `re_render`), a merged crate's tests, or a probe in this container. The source is named.
- **Provisional**: the version exists on crates.io, is not yanked and resolves in a workspace lockfile, but it (or the listed feature set) has not been built and exercised yet. We confirm or replace these crates in M1. Their versions are exact all the same.

All versions below were checked against the crates.io index on 2026-10-14. On that date none was yanked, and each was the newest release unless noted.

## Toolchain

| Item | Value |
|---|---|
| Toolchain file | `rust-toolchain.toml`: `channel = "stable"`, components `rustfmt`, `clippy` |
| Workspace `rust-version` | `1.95` |
| What sets the MSRV | `egui` / `egui-wgpu` / `egui-winit` 0.36.2 and `bevy_ecs` 0.20.0 need 1.95. The next highest are `puffin` 0.20.0 (1.92, optional), then `ordered-float` 5.5.0 (1.90, transitive), `uuid` 1.28.0 and `smol_str` 0.3.6 (1.89), and `oxijolt`, `mlua`, `rmcp` and `image` (1.88). |
| Edition | 2024 |
| Toolchains seen working | rustc 1.97.0 and 1.99.0 in the development containers. The render prototype declared `rust-version = "1.95"`; the core spike declares none. |
| Dev profile | `debug = "line-tables-only"`. The render spike measured this as a large saving in link time and disk use. |

The `msrv` CI job builds with exactly 1.95.0 (`rustup toolchain install 1.95.0 --profile minimal`), so that the MSRV stays honest. Everything else, locally and in CI, uses the current stable toolchain.

## Runtime dependencies

### Rendering, windowing, UI

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| wgpu | 30.0.1 | MIT OR Apache-2.0 | 1.87 | GPU abstraction. Vulkan, Metal, DX12 and GL backends. | default | render prototype, then `re_render`'s tests on lavapipe and llvmpipe | Verified |
| winit | 0.30.13 | Apache-2.0 | 1.70 | Window and event loop. A normal dependency of `re_input` (key and mouse types); only a dev-dependency of `re_render`, for its `windowed_smoke` example (`re_render` itself takes any window handle). `re_app` will own the event loop. | default | render prototype `windowed`, then `re_render`'s `windowed_smoke` under Xvfb | Verified |
| web-time | 1.1.0 | MIT OR Apache-2.0 | 1.60 | The wall clock (`Instant`, `SystemTime`) re-exported by `re_core::time`: `std::time` on desktop, the browser's clock on wasm32, where `std::time::Instant::now` panics (ADR-0049 §5). Already in the lockfile through winit and egui. | default | WP29 spike and PR "portable" (#89, the `build-web` job) | Verified |
| wasm-bindgen | 0.2.129 | MIT OR Apache-2.0 | 1.81 | The JavaScript bindings of the browser build (`re_web`, `tools/web-test`; wasm32 only). Already in the lockfile through wgpu and winit; `wasm-bindgen-cli` must be the same version. | default | WP29 PRs "toolchain" (#99) and "runtime" (`build-web`: Node and headless Chromium runs) | Verified |
| wasm-bindgen-futures | 0.4.79 | MIT OR Apache-2.0 | 1.81 | Awaits the browser's promises: the WebGPU adapter and device (`re_app/src/web/play.rs`) and `re_web`'s async exports. wasm32 only; already in the lockfile through wgpu. | default | WP29 PR "runtime" (`build-web`) | Verified |
| egui | 0.36.2 | MIT OR Apache-2.0 | 1.95 | Immediate-mode UI for the HUD, debug overlay and editor-lite | default (its `default_fonts` pulls `epaint_default_fonts`: OFL-1.1 and Ubuntu-font-1.0, an ADR-0019 exception) | render prototype `gui_offscreen` | Verified |
| egui-wgpu | 0.36.2 | MIT OR Apache-2.0 | 1.95 | egui renderer on our wgpu device | default | render prototype | Verified |
| egui-winit | 0.36.2 | MIT OR Apache-2.0 | 1.95 | egui input from winit. Only a dev-dependency of `re_render` so far (its `windowed_smoke` example); `re_app` and `re_editor` will use it. | default (its `clipboard` feature pulls `arboard`, and on Windows `clipboard-win` and `error-code`, BSL-1.0) | render prototype, then `re_render`'s `windowed_smoke` | Verified |
| glam | 0.34.1 | MIT OR Apache-2.0 | 1.85 | Math (Vec3, Quat, Mat4, Affine3A) | `bytemuck`, `libm`, `mint-05`, `scalar-math`, `serde`. `libm` and `scalar-math` since WP18 PR "d2" (ADR-0036, ADR-0049 §11): glam's own trigonometry goes through libm, and its maths is the same scalar code on every target, so x86_64, arm64 and wasm agree bit for bit | render prototype (`bytemuck`), platform spike (`mint-05`), scripting spike (default); WP0 builds the combined feature set and round-trips every `re_types` contract type through serde (`crates/re_types/tests/roundtrip.rs`) | Verified |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT | none | POD casts for GPU buffers | `derive` | render prototype | Verified |
| pollster | 1.0.1 | Apache-2.0 OR MIT | 1.69 | Blocks on wgpu futures (adapter, device, map_async) | default | render prototype | Verified |
| png | 0.18.1 | MIT OR Apache-2.0 | 1.73 | Writes golden images and readback output | default | render prototype | Verified |

There is no `eframe`. We own the wgpu device and pass `(&Device, &Queue, &TextureView)` to both the renderer and egui, so the headless and windowed paths are the same code (ADR-0010).

### Physics

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| oxijolt | 1.1.0+jolt-5.6.0 (pin `=1.1.0`) | MIT OR Apache-2.0 | 1.88 | Rust bindings to Jolt Physics 5.6.0. Bodies, shapes, constraints, CharacterVirtual, queries, sensors, state save. | `default-features = false`, `cross-platform-deterministic` (always on since WP18 PR "d2", ADR-0036) | physics spike, 12 scenarios plus a bench; source build about 55 s | Verified |

Notes:
- The available features are `asserts`, `bindgen`, `cross-platform-deterministic`, `debug-renderer`, `double-precision`, `glam032`, `mint` and `prebuilt`. We leave `glam032` off, because it would pull a third glam version. Conversion is a two-line function in `re_physics`.
- A source build needs `cmake` and a C++17 compiler (see System packages). It does not need libclang: `oxijolt-sys` ships committed bindings for the Linux, Windows and macOS targets, and `bindgen` is an optional build dependency behind its `bindgen` feature, which we leave off (physics spike §1).
- The crate has few downloads (464 when checked). We pin it exactly and hide it entirely behind `re_physics` (ADR-0008).

### Scripting and script tooling

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| mlua | 0.12.2 | MIT | 1.88 | Embeds Luau (vendored Luau 0.740 through mlua-sys 0.13.0 and luau0-src 0.22.0+luau740; no Analysis module) | `luau` only. Not `vendored` (it does nothing for Luau) and not `send`. `luau-jit` only through `re_script`'s opt-in `jit` feature. | scripting spike: 14 feature tests in the default, `send` and `jit` builds, benchmarks, and a corpus compile | Verified |
| full_moon | 3.0.0 | MPL-2.0 | none | Luau parser for the static API-gap scan (validation tier 2). Tooling only (`re_script_check`). | `luau` | design-phase probe in this container: `parse_fallible(code, LuaVersion::luau())` parsed an external Roblox-style corpus with no failures | Verified; MPL exception (ADR-0019) |

### Place-file import (M3, tooling only)

Used only by `re_import`, a tooling crate, so none of them ships in a packaged game (ADR-0048). Versions checked against crates.io on 2026-10-15.

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| lz4_flex | 0.14.0 | MIT | 1.81 | LZ4 block bodies of binary place-file chunks | `default-features = false`, `std`, `safe-decode`, `checked-decode` | WP27 probe in this container (LZ4 block round trip) | Verified (probe); lands with WP27 PR "place files" |
| ruzstd | 0.9.1 | MIT | 1.87 | ZSTD frame bodies of binary place-file chunks, pure Rust | `default-features = false`, `std` | WP27 probe (ZSTD frame decode) | Verified (probe); lands with WP27 PR "place files" |
| quick-xml | 0.41.0 | MIT | 1.79 | `.rbxlx`/`.rbxmx` reader | defaults | WP27 probe; already in the lockfile through `wayland-scanner`, so no new duplicate | Verified (probe); lands with WP27 PR "place files" |
| base64 | 0.23.1 | MIT OR Apache-2.0 | 1.71 | binary and shared strings in XML place files | defaults | WP27 probe; already in the lockfile through `rmcp` | Verified (probe); lands with WP27 PR "place files" |

### Polygon booleans and roads (M3, runtime)

Used only inside `re_geom::polygon`, never in its public types (ADR-0043). Pinned exactly; an upgrade that moves any `re_geom` golden needs node version bumps (ADR-0043, section 1). Checked against crates.io on 2026-10-15. MSRVs are confirmed by the `msrv` job when the PR lands.

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| i_overlay | 9.1.0 | MIT OR Apache-2.0 | 1.88 | integer-core polygon booleans, offsets and strokes on a fixed 1 mm grid | `default-features = false` | WP24 `overlay-probe` (road strokes, union and intersection, 5,000-square union; identical hashes across runs); `re_geom` polygon tests and goldens | Verified; landed with WP24 PR "booleans and roads" |
| i_float, i_shape, i_key_sort, i_tree | 5.0.0, 5.0.0, 0.12.0, 0.20.0 | MIT | ≤ 1.88 | i_overlay's own building blocks (same author) | defaults | WP24 probe (resolved with i_overlay) | Verified; transitive |
| earcut | 0.4.11 | MIT OR Apache-2.0 | ≤ 1.95 | triangulating road unions, junction caps and shapes | defaults | WP24 probe (240-point road union, area matches); `re_geom` triangulation tests and goldens | Verified; landed with WP24 PR "booleans and roads" |

`libm` 0.2.16 and `num-traits` 0.2.19, which these pull, are already in the lockfile.

### Block-compressed textures (M3)

ADR-0047. Checked against crates.io on 2026-10-15; `cargo deny check` passes with no exception.

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| block_compression | 0.10.1 | MIT | 1.80 | BC7/BC5 encoder at import (`re_assets`, feature `texture-encode`, turned on by `tooling`; tooling only, forbidden in the runtime build by `check-graph`); dev-dependency of `re_render` for decoder cross-checks | `default-features = false`, `bc15`, `bc7` | WP20 phase-0 bench (encode, decode, GPU round trip on lavapipe and llvmpipe GL) | Verified (probe); lands with WP20 PR "textures" |
| bcdec_rs | 0.2.0 | MIT | ≤ 1.95 | BC decoder for adapters without `TEXTURE_COMPRESSION_BC` (runtime, `re_render`) | defaults (no dependencies) | WP20 bench (bit-identical to the GPU round trip on both backends) | Verified (probe); lands with WP20 PR "textures" |

### Package archive (M3, runtime)

Used by `re_app::package` to read and write `game.rpak` (ADR-0037). With `default-features = false` it builds the bundled libzstd with `cc` (no pkg-config, no system library) and drops the legacy decoders and the dictionary builder. Checked against crates.io on 2026-10-15.

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| zstd | 0.14.2 | BSD-3-Clause | 1.64 | per-entry and index compression in `game.rpak` | `default-features = false` | WP19 measurements (decode 400–700 MB/s at levels 3–9; `ldd` shows only libc and libgcc_s) | Verified (probe); lands with WP19 PR "archive" |
| zstd-safe, zstd-sys | 8.1.0, 2.1.1+zstd.1.5.7 | BSD-3-Clause | ≤ 1.64 | zstd's bindings and the vendored C library | defaults of `zstd` | WP19 measurements | Verified (probe); transitive |

blake3, `image` and `windows-sys` 0.61.2 (Windows only) are already in the lockfile; `re_cli` takes `image` and `windows-sys` as direct dependencies in WP19's PRs.

### Editor node canvas and docking (M3, tooling only)

Used only by `re_editor`, a tooling crate, so the runtime-only binary does not link them (ADR-0044, ADR-0027). Pinned exactly; an egui-snarl upgrade re-checks its list of direct edits (ADR-0044). Checked against crates.io on 2026-10-15. With them the lockfile gains three transitive crates, all MIT OR Apache-2.0 and none a new duplicate: egui-scale 0.6.1, duplicate 2.0.1 and proc-macro2-diagnostics 0.10.1. ahash, slab, smallvec, paste, heck and thiserror 2 are already in the lockfile. Gizmos are our own code; transform-gizmo-egui is not used (ADR-0044).

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| egui-snarl | 0.12.0 | MIT OR Apache-2.0 | 1.92 | node canvas for ProcGraph (and later AnimGraph), as a read-only view-model | `serde` | WP25 `cratecheck` (built with egui 0.36.2, one egui in the tree) | Verified (probe); lands with WP25's canvas PR |
| egui_dock | 0.21.1 | MIT | 1.95 | dockable editor panels with floating windows; layout saved as UI state | `serde` | WP25 `cratecheck` | Verified (probe); lands with WP25's docking PR |

### MCP over HTTP (M3, tooling only)

Used only by `re_mcp`, a tooling crate, so the runtime-only binary does not link them (ADR-0046, ADR-0027). rmcp 3.5.1 gains its `transport-streamable-http-server` feature and tokio gains `net`. With them the lockfile gains 13 transitive crates, all under the license allow-list and none a new duplicate: http 1.5.0, http-body 1.1.0, http-body-util 0.1.5, httparse 1.10.1, httpdate 1.0.3, tower-service 0.3.3, sse-stream 0.2.6, tokio-stream 0.1.19, async-trait 0.1.92, rand 0.10.3, rand_core 0.10.1, chacha20 0.10.2 and socket2 0.6.5.

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| hyper | 1.12.0 | MIT | ≤ 1.85 | HTTP/1.1 server under rmcp's `StreamableHttpService` for `real mcp --http` | `default-features = false`, `http1`, `server` | WP26 prototype in this container (401, 403, both protocol revisions, policy refusals) | Verified (probe); lands with WP26 PR "http" |
| hyper-util | 0.1.21 | MIT | ≤ 1.85 | `TowerToHyperService` and the tokio glue | `default-features = false`, `tokio`, `server`, `http1`, `service` | WP26 prototype | Verified (probe); lands with WP26 PR "http" |

### External tools (not linked, not from crates.io)

| Tool | Version | License | Role | Verified by | Status |
|---|---|---|---|---|---|
| luau-lsp | 1.70.1 | MIT | The default type checker for `real check --types` (`luau-lsp analyze`), and the language server for editors and agents. Its Luau submodule is the commit of Luau 0.740. | scripting spike: `analyze` with `--definitions` and `--flag:LuauSolverV2=true`; LSP over stdio. Linux release zip only. | Verified (Linux) |
| luau-analyze | 0.740, plus a 27-line `--definitions` patch | MIT | Alternative checker (`[scripts] checker`). Built from the Luau 0.740 tag. | scripting spike: built from source in 220 s, checked the spike's typed fixtures | Verified (Linux); needs a rebase per Luau upgrade |
| cargo-deny | 0.20.2 | MIT OR Apache-2.0 | The `deny` CI job (`cargo install cargo-deny --version 0.20.2 --locked`) | WP0: `cargo deny check` on this tree passes (advisories, bans, licenses, sources ok; `unmatched-skip` and `unused-wrapper` warnings only for crates not yet in the tree) | Verified |
| wasi-sdk | 34 | Apache-2.0 WITH LLVM-exception | Clang, wasi-libc and the exception-enabled libc++ that build Luau and Jolt for `wasm32-unknown-unknown` (`tools/ci/wasi-sdk.sh`, sha256-pinned) | WP29 PR "toolchain" (#99) | Verified (Linux) |
| wasm-bindgen-cli | 0.2.129 | MIT OR Apache-2.0 | Writes the browser build's JavaScript glue (`cargo install --locked`), matching the lockfile's wasm-bindgen | WP29 PR "toolchain" (#99) | Verified |
| binaryen (`wasm-opt`) | 133 | Apache-2.0 | Shrinks the browser build to its 8 MB gzip budget (`tools/ci/binaryen.sh`, sha256 from the release's own file) | WP29 PR "runtime": 18.2 MB, 6.9 MB gzip, same `state_hash` | Verified (Linux) |
| Node.js | 22 | MIT | Runs the wasm checks of `build-web` (`actions/setup-node`) | WP29 PRs "toolchain" and "runtime" | Verified |
| Playwright (npm) | 1.56.1, with its Chromium | Apache-2.0 | `tools/web-test` only: drives headless Chromium with WebGPU in `build-web`. Pinned in `tools/web-test/package.json` and its lockfile (`npm ci`, `npx playwright install --with-deps chromium`). Not part of any build of the engine. | WP29 PR "runtime" | Verified (Linux) |

These are tools, like cmake. The crates.io-only rule covers code dependencies. CI pins each tool's version, and the downloaded luau-lsp release by its sha256.

### Bundled assets (files, not crates)

| Asset | Version | License | Role | Source | Status |
|---|---|---|---|---|---|
| Montserrat, weights 400, 500, 700, 900 (latin subset, TTF) | `@fontsource/montserrat` 5.3.0 | OFL-1.1 | Compat UI substitute for geometric display families (ADR-0035) | npm package, WOFF 1.0 unpacked to TTF by `crates/re_compat/assets/fonts/tools/woff2ttf.py`; glyph data unchanged | Verified (WP15, #61) |
| Source Sans 3, weights 400, 700 (latin subset, TTF) | `@fontsource/source-sans-3` 5.3.1 | OFL-1.1 | Compat UI default and substitute for sans families (ADR-0035) | as above | Verified (WP15, #61) |
| Roboto Mono, weights 400, 700 (latin subset, TTF) | `@fontsource/roboto-mono` 5.3.0 | OFL-1.1 | Compat UI substitute for monospace families (ADR-0035) | as above | Verified (WP15, #61) |

The files live in `crates/re_compat/assets/fonts/`, about 307 KB in all, each family next to its `OFL-*.txt`. `substitutes.json` maps compat font families to them. They are files rather than crates, so the crates.io-only rule does not apply, and `cargo deny` does not see them; their licenses travel with them and go into the third-party notice.

### Core, data and serialization

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| bevy_ecs | 0.20.0 (released 2026-10-08; pin `=0.20.0`) | MIT OR Apache-2.0 | 1.95.0 | Entity storage, hierarchy (`ChildOf` / `Children`), hooks, change ticks and runtime-defined components, used standalone inside the model crates (`re_reflect`, `re_scene`, `re_command`) behind our own API (ADR-0005) | `default-features = false`, `std`, `multi_threaded`. Not `bevy_reflect` (ADR-0032), `async_executor`, `backtrace`, `debug` or `serialize`. | core spike: a 2,600-line prototype with 15 passing tests and a 100k-entity benchmark; five-crate comparison | Verified |
| rayon | 1.12.0 | MIT OR Apache-2.0 | 1.80 | The one worker pool. Shared with Jolt through `oxijolt::JobSystem`. | default | WP0: `re_core::TaskPool` (`crates/re_core/tests/task.rs`: named `re-worker-N` threads, `spawn`, `scope`, the global pool). WP2: the rayon adapter for Jolt, `re_physics::jobs` (`crates/re_physics/tests/job_threads.rs`: only `re-worker-N` threads and the caller run Jolt jobs) | Verified |
| libm | 0.2.16 | MIT | 1.63 | Portable transcendental maths (sin, atan2, pow, ...): the same bits on every platform, unlike the C library std calls. Every engine call goes through it, Luau's `math` library included, and a root `clippy.toml` bans std's versions (ADR-0036). | default | WP18 PR "d2": `crates/re_script/tests/portable_math.rs` (Luau's maths give libm's bits; an input where glibc rounds differently), the `d2` CI job (the same replay hashes on Linux and Windows); already in the lockfile through naga and num-traits | Verified |
| serde | 1.0.229 | MIT OR Apache-2.0 | 1.56 | Serialization traits | `derive` | AI spike | Verified |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | 1.71 | Scene, prefab, graph and material files; command and log JSON; CLI JSON output | `preserve_order`, `float_roundtrip` | AI spike (default features); core spike (`float_roundtrip`: exact float parsing, byte-identical scene round trips); WP0 (`preserve_order`: graph parameters keep file order, test `graph` in `crates/re_types/tests/roundtrip.rs`) | Verified |
| schemars | 1.2.3 | MIT | 1.74 | JSON Schema for MCP tool inputs and the statically typed file formats (materials, graphs, verify report, agent policy). Scene and prefab schemas come from the component registry instead (ADR-0016). | `derive` | AI spike (MCP tool schemas); WP0 `re_types` optional `schemars` feature (`GraphDoc`, `ReplayDoc`; `crates/re_types/tests/schema.rs`); WP5 `re_assets` optional `schemars` feature (`MaterialFile` and `AlphaModeName`, for `material.schema.json`; `crates/re_assets/tests/schema.rs`) | Verified |
| toml | 1.1.8+spec-1.1.0 | MIT OR Apache-2.0 | 1.85 | `real.toml`, `.real/agent-policy.toml` | default | WP0: `cargo xtask dupes` reads `deny.toml` with it (test `reads_every_skip_form` in `tools/xtask/src/dupes.rs`) | Verified |
| postcard | 1.1.3 | MIT OR Apache-2.0 | none | Binary cache blobs (processed meshes, EvalReport cache). Replaces bincode. | `use-std` | WP4 and WP5: `re_proc`'s node cache and `re_assets`' processed cache round-trip through it (their crate tests) | Verified |
| blake3 | 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | none | Asset cache keys, ProcGraph node keys, `state_hash` | default | platform spike (`assets.rs` cache key) | Verified |
| uuid | 1.28.0 | Apache-2.0 OR MIT | 1.89 | Asset ids (`.meta.json`). Entity ids are short strings made by `re_scene::IdGen`, not uuids. | `v4`, `serde`, `js` (randomness from the browser on wasm32, ADR-0049) | WP5: `re_assets` sidecar ids (`.meta.json`, its import tests) | Verified |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | 1.85 | Ordered maps in documents, commands and the registry (deterministic file output) | `serde` | core spike | Verified |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | 1.77 | Library error types | default | WP0, WP4, WP5: `re_reflect`, `re_scene`, `re_command`, `re_geom`, `re_proc`, `re_assets` | Verified |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | 1.56 | Token streams for `re_reflect_derive` (compile time only) | default | `re_reflect_derive` tests (`re_reflect/tests/derive.rs`); already in the lockfile through bevy_ecs_macros | Verified |
| quote | 1.0.47 | MIT OR Apache-2.0 | 1.56 | Code generation for `re_reflect_derive` (compile time only) | default | as above | Verified |
| syn | 2.0.119 | MIT OR Apache-2.0 | 1.61 | Parsing the derive input of `re_reflect_derive` (compile time only) | default | as above | Verified |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | 1.68 | Binaries only (`real`, tools) | default | WP0: `tools/xtask` (`cargo xtask check` and its unit tests) | Verified |

### Assets, input, audio, platform

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| gltf | 1.4.1 | MIT OR Apache-2.0 | 1.61 | glTF 2.0 import | `default-features = false`, `import`, `utils`, `names`, `KHR_materials_emissive_strength` | platform spike `gltf_import` tests | Verified |
| image | 0.25.10 | MIT OR Apache-2.0 | 1.88 | Texture decode | `default-features = false`, `png`, `jpeg` | platform spike | Verified |
| notify | 8.2.0 | CC0-1.0 | 1.77 | File watching for hot reload | default | platform spike `watch` tests | Verified |
| notify-debouncer-full | 0.7.0 | MIT OR Apache-2.0 | 1.85 | Debounced file events | default | platform spike | Verified |
| kira | 0.12.5 | MIT OR Apache-2.0 | none | Audio playback and mixing | `default-features = false`, `cpal`. Not `wav`: it enables symphonia's RIFF container without the PCM codec, so it cannot decode WAV anyway; `re_audio` has its own WAV reader | platform spike (`CaptureBackend`, headless fallback); WP5 (`re_audio` tests) | Verified. Pulls MPL-2.0 `triple_buffer` (always); see policy 4 and ADR-0019 |
| gilrs | 0.11.2 | Apache-2.0 OR MIT | 1.84 | Gamepads | **default features required** | platform spike | Verified |
| tracing | 0.1.44 | MIT | 1.65 | Logging and diagnostics events | default | platform spike | Verified |
| tracing-subscriber | 0.3.23 | MIT | 1.65 | Log output (human and JSON) | `env-filter`, `fmt`, `ansi`, `json` | platform spike; WP0 `re_core::init_logging` (`crates/re_core/tests/log.rs`: filters, JSON lines) | Verified |
| puffin | 0.20.0 | MIT OR Apache-2.0 | 1.92 | In-process profiler behind `profile_scope!` | optional, feature `profile-puffin` | platform spike; WP0 `re_core` (`cargo test -p re_core --all-features`; CI clippy builds all features) | Verified |
| tracy-client | 0.19.0 | MIT OR Apache-2.0 | 1.70 | Tracy profiler behind `profile_scope!` | optional, `default-features = false`, `enable`, `ondemand` | platform spike; WP0 `re_core` (`cargo test -p re_core --all-features`; CI clippy builds all features) | Verified |

### Tools, CLI, agent

| Crate | Version | License | MSRV | Role | Features | Verified by | Status |
|---|---|---|---|---|---|---|---|
| clap | 4.6.7 | MIT OR Apache-2.0 | 1.85 | `real` CLI | `derive` | WP6: `re_cli` (`crates/re_cli/tests/cli.rs`, including usage errors exiting 2) | Verified |
| rmcp | 3.5.1 | Apache-2.0 | 1.88 | MCP server (stdio) | `server`, `transport-io`, `macros` | AI spike: 12 stdio tests over two protocol revisions; driven end to end by Claude Code | Verified |
| tokio | 1.53.3 | MIT | 1.71 | Async runtime for `re_mcp` **only** | `macros`, `rt-multi-thread`, `io-std` | AI spike | Verified |

## Dev-dependencies

| Crate | Version | License | MSRV | Role | Status |
|---|---|---|---|---|---|
| insta | 1.49.0 | Apache-2.0 | 1.66 | Snapshot tests for JSON output, diagnostics and command specs (`json` feature) | Verified (WP3, WP7: snapshot tests in `re_script`, `re_script_check` and `re_agent`) |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | 1.63 | Temporary project directories in tests | Verified (WP0: the `tools/xtask` explain tests build fixture workspaces in temp dirs) |

## Known duplicate versions

These come from `cargo tree -d -e normal --target <triple>` for the three desktop triples on a resolve-only lockfile that contains the whole M1 set with the features above (2026-10-14). They are accepted, and listed in `deny.toml` under `[bans] skip` as (name, version) pairs with these reasons. "Runtime" means the crate is linked into a binary; "compile-time" means it is only used by a proc-macro.

| Crate | Versions | Pulled by | Kind | Action |
|---|---|---|---|---|
| glam | 0.33.12 and 0.34.1 | kira 0.12.5 (0.33) | runtime | Accept. Never pass kira's glam types across our API; `re_audio` converts with plain arrays. Drop the duplicate when kira moves to 0.34. |
| hashbrown | 0.16.1 and 0.17.1 | gpu-allocator → wgpu-hal and bevy_platform (0.16); indexmap 2 → naga, wgpu and others (0.17) | runtime | Accept. Drop when those move to one version. |
| hashbrown, indexmap | 0.12.3 / 1.9.3 next to 0.16–0.17 / 2.14.2 | full_moon_derive | compile-time | Accept |
| syn | 1.0.109, 2.0.119, 3.0.9 | full_moon_derive (1); re_reflect_derive, gltf-derive, tracing-attributes, thiserror-impl, zerocopy-derive, derive_more-impl, bevy_ecs_macros (2); bytemuck_derive, clap_derive, darling (3) | compile-time | Accept |
| derive_more, derive_more-impl | 1.0.0 and 2.1.1 | full_moon (1.0, tooling only); bevy_ecs and bevy_tasks (2.1) | runtime (2.1); tooling (1.0) | Accept. Drop when full_moon moves to derive_more 2. |
| thiserror, thiserror-impl | 1.0.69 and 2.0.21 | calloop 0.13 (through winit's Wayland stack) | runtime | Accept |
| getrandom | 0.3.4 and 0.4.3 | ahash through egui (0.3); uuid (0.4) | runtime | Accept |
| base64 | 0.13.1 and 0.23.1 | gltf (0.13); rmcp (0.23) | runtime | Accept |
| calloop (0.13 / 0.14), calloop-wayland-source (0.3 / 0.4), smithay-client-toolkit (0.19 / 0.20), rustix (0.38 / 1.1), linux-raw-sys (0.4 / 0.12) | two each | winit's Wayland stack | runtime | Accept |
| rustc-hash | 1.1.0 and 2.1.3 | naga, wgpu-core (1); mlua, type-map (2) | runtime | Accept |
| itertools | 0.14.0 and 0.15.0 | puffin (0.14, optional); egui (0.15) | runtime | Accept |
| smol_str | 0.2.2 and 0.3.6 | winit (0.2); full_moon (0.3) | runtime | Accept |
| miniz_oxide | 0.8.9 and 0.9.1 | png (0.8); flate2 (0.9) | runtime | Accept |
| spin | 0.9.9 and 0.12.3 | heapless through postcard (0.9); bevy_platform (0.12) | runtime | Accept |
| windows-sys (0.52 / 0.60 / 0.61), windows-targets (0.52 / 0.53) and the matching `windows_x86_64_msvc` | several each | winit (0.52) and the rest of the Windows stack | runtime, Windows only | Accept. The usual Windows ecosystem split; recheck on every upgrade. |
| bitflags | 1.3.2 and 2.13.2 | core-graphics 0.23 through winit (1.3); everything else (2) | runtime, macOS only | Accept. Drop when winit leaves core-graphics 0.23. |
| objc2, block2, objc2-foundation, objc2-app-kit | 0.5 / 0.6 (block2), 0.5.3 / 0.6.5 (objc2), 0.2.2 / 0.3.2 (the frameworks) | winit 0.30 (older line); wgpu-hal, cpal and arboard (newer line) | runtime, macOS only | Accept. Drop when winit moves to objc2 0.6. |

Not duplicates: on the Linux host, `serde_core` 1.0.229, `serde_json` 1.0.154, `toml_datetime` 1.1.2, `toml_parser` 1.1.5 and `winnow` 1.0.4 each appear twice at the **same** version (host and target builds with different features); `serde` 1.0.229 and `memchr` 2.8.3 do the same on other targets. `cargo xtask dupes` ignores same-version pairs. Duplicates on targets outside the three desktop triples (`--target all` also shows `jni-sys`, `r-efi`, `redox_syscall`, `heapless`/`hash32`, `objc2-ui-kit` and more for Android, UEFI, Redox and iOS) are out of scope until those platforms are, which is why `[graph] targets` is limited.

Never accepted: a second `wgpu`, `winit`, `egui` or `mlua` version, or a second glam inside our own crates. `deny.toml` bans those names from appearing twice.

## Considered and not used in M1

| Crate | Reason |
|---|---|
| bincode | Not used. postcard replaces it (ADR-0016). |
| luau-analyze 0.0.1, ruau 0.4.0 | Rejected by the scripting spike: few downloads, unproven, and they either re-implement the checker or link Luau Analysis next to mlua. Type checking uses the official tools out of process (ADR-0013). |
| eframe | We own the device and loop (ADR-0010). |
| profiling 1.0.18 | Pins tracy-client ^0.18, which conflicts with 0.19. We write our own `profile_scope!` macro (ADR-0021). |
| crossbeam-channel, parking_lot, smallvec | std is enough for M1 |
| naga_oil 0.23.0 | Our own `#include` preprocessor is enough for a small shader set |
| egui_dock 0.21.1 | egui panels are enough for editor-lite. Taken in M3 (ADR-0044). |
| transform-gizmo-egui 0.11.0 | Gizmos are cut from M1. It also needs Rust 1.96 and pulls glam 0.32; M3 writes its own gizmos (ADR-0044). |
| i_overlay 9.1.0 | Deferred. Published on 2026-10-14, the day we checked, and not needed until polygon boolean nodes (M3). |
| meshopt 0.6.2, spade, manifold3d, egui-snarl, zstd, libm | Not needed in M1. egui-snarl (ADR-0044) and zstd (ADR-0037) are taken in M3. |
| hecs 0.11.2, sparsey 0.13.4, shipyard 0.11.5, flecs_ecs 0.2.2 | Entity storage alternatives measured by the core spike; rejected in ADR-0005 (no hierarchy, hooks or per-component change ticks in hecs and sparsey; no hierarchy in shipyard; flecs is C and segfaulted from safe Rust) |
| bevy_reflect 0.20.0 (bevy_ecs's default `bevy_reflect` feature) | Reflecting glam types needs bevy_reflect's optional `glam` feature, which ties it to glam 0.33.x (0.33.12 resolved; bevy_ecs's `bevy_reflect` feature does not turn it on by itself); no f64 → f32 coercion, `ReflectComponent::apply` panics on a type mismatch; rejected in ADR-0032 |
| ron 0.12.2 | Scene format runner-up; its `deserialize_any` path is quadratic (ADR-0016). Used only in the core spike. |
| syn 3.0.9, quote 1.0.47, proc-macro2 1.0.107 as direct dependencies | Only needed for a `#[derive]` component macro, which is later work; M1 uses the `component!` macro (ADR-0032) |
| rapier3d, rhai, rlua | See DECISIONS for the comparisons |

## System packages

### Linux (Debian/Ubuntu, used by CI and the dev container)

```
apt-get install -y \
  build-essential g++ cmake pkg-config \
  libvulkan-dev mesa-vulkan-drivers vulkan-tools \
  libegl1 libegl-mesa0 libgl1-mesa-dri libgles2 \
  libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libxcursor-dev libxrandr-dev libxi-dev libwayland-dev \
  libasound2-dev libudev-dev \
  xvfb
```

- `mesa-vulkan-drivers` provides lavapipe, the software Vulkan driver that all headless GPU tests use. llvmpipe (through the GL backend) is the second software path.
- `libxkbcommon-x11-dev` is required at runtime: winit panics without libxkbcommon-x11.
- `libegl1`, `libegl-mesa0`, `libgl1-mesa-dri` and `libgles2` are for wgpu's GL backend on headless Linux (EGL with Mesa's surfaceless platform, llvmpipe). Without them `RE_BACKENDS=gl` and the GL goldens fail on a fresh machine.
- `vulkan-tools` gives `vulkaninfo`, which CI prints so that the Mesa version behind the goldens is in the log.
- `cmake` and `g++` are for oxijolt's source build of Jolt. No libclang is needed (see Physics).
- `libasound2-dev` is for cpal (kira). With no sound card, the ALSA `null` device works, and `Audio::new_or_headless` falls back to the capture backend.
- `libudev-dev` is for gilrs.
- `xvfb` is only for the windowed smoke tests.

### macOS

Xcode command line tools and `cmake` (Homebrew). wgpu uses Metal. **CI (`macos-latest`) builds the workspace and runs the tests of the pure-Rust crates (`re_core`, `re_noise`, `re_types`, `re_reflect`, `re_scene`, `re_command`); the result is reported but does not block a merge. Not yet verified.**

### Windows

Visual Studio 2022 Build Tools with the C++ workload and `cmake`. wgpu uses DX12 or Vulkan. **CI (`windows-latest`) builds the workspace and runs the same pure-Rust tests as macOS; the result is reported but does not block a merge. Not yet verified.** luau-lsp publishes Windows and macOS release zips; they have not been tried. The raw Luau interrupt (ADR-0011) must be checked on MSVC before Windows blocks merges.
