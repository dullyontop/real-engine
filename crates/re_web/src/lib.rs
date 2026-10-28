//! `re_web`: Real Engine in the browser (ADR-0049).
//!
//! The boot script hands the game's archive to [`Session::add_archive`] (a
//! folder `real build --target web` wrote), or a project's loose files to
//! [`Session::add_file`], and then plays it with [`Session::open_for_play`]
//! (`play` in JavaScript).
//! [`Session::run_replay`] is the headless entry point CI uses: it plays one
//! replay without a GPU, as `real verify` does, so the `state_hash` can be
//! compared with desktop's (equal once Jolt and glam are deterministic across
//! platforms, ADR-0036). [`Session::run_replay_gpu`] plays it with a GPU
//! device, drawing frames offscreen, and reports the GPU's errors too.
//!
//! A Roblox-style game joins a server ([`Session::open_for_play`] with an
//! address): a browser is always a network client, over a WebSocket (ADR-0050's
//! amendment for PR "web"). [`NetRun`] plays a replay as a lockstep client,
//! a step at a time between turns of the page's event loop, which is how CI
//! checks a browser against a desktop server.

#![warn(missing_docs)]

use std::sync::Arc;

use re_app::package::{ARCHIVE_NAME, Archive};
use re_app::{App, AppConfig, Graphics, NetOptions, NetRole};
use re_core::files::{
    FileLayer, Form, LayerKind, MemLayer, ProjectFiles, ProjectPath, WritableDirs,
};
use re_render::{Capture, Gpu, wgpu};

/// The project files received so far, and what to play.
#[derive(Debug)]
pub struct Session {
    archive: Option<Arc<Archive>>,
    files: Arc<MemLayer>,
    saved: Vec<(String, Vec<u8>)>,
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

impl Session {
    /// An empty project.
    pub fn new() -> Session {
        Session {
            archive: None,
            files: Arc::new(MemLayer::new(LayerKind::Project)),
            saved: Vec::new(),
        }
    }

    /// Adds the project file at `path` (project-relative, `/`-separated).
    ///
    /// # Errors
    ///
    /// The path's problem when it is not a valid project path, or a project
    /// opened from this session is still running.
    pub fn add_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let rel = ProjectPath::new(path).map_err(|e| format!("{path}: {e}"))?;
        let files = Arc::get_mut(&mut self.files)
            .ok_or_else(|| format!("{path}: the project is already running"))?;
        files.insert(rel, bytes.to_vec(), Form::Source);
        Ok(())
    }

    /// Reads the game from `bytes`, a `game.rpak` (ADR-0049 §6). Its files come
    /// before any added with [`Session::add_file`], as an archive comes before
    /// the folder that holds it in a desktop game.
    ///
    /// # Errors
    ///
    /// The diagnostic, rendered, when `bytes` is not an archive this runtime
    /// reads.
    pub fn add_archive(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let archive = Archive::from_bytes(ARCHIVE_NAME, bytes).map_err(|d| d.to_string())?;
        self.archive = Some(Arc::new(archive));
        Ok(())
    }

    /// The name the game's saved data is kept under: the archive's
    /// `[package] identifier`, or for loose files the project's name.
    ///
    /// # Errors
    ///
    /// As [`Session::project_name`].
    pub fn game_id(&self) -> Result<String, String> {
        match &self.archive {
            Some(a) => Ok(a.index().package.identifier.clone()),
            None => self.project_name(),
        }
    }

    /// Adds a file an earlier run persisted, keyed as
    /// [`re_app::web::Sink`] receives it (`data/<path>` or `crash/<path>`).
    pub fn add_saved(&mut self, key: &str, bytes: &[u8]) {
        self.saved.push((key.to_owned(), bytes.to_vec()));
    }

    /// Opens the project for play, with the saved files added so far and every
    /// later change of saved data and crash reports sent to `sink`. A compat
    /// game keeps its data as `real run` does ([`App::persist_data`]), in the
    /// saved files. With `connect`, a Roblox-style game joins the server there
    /// (`ws://host:port/`, or `host:port`) as `name`, and keeps no data: the
    /// server's is the game's.
    ///
    /// # Errors
    ///
    /// The diagnostic when the project does not open, or does not go online
    /// (`RE8909` not a Roblox-style project, `RE8910` an address the browser
    /// refuses).
    pub fn open_for_play(
        &self,
        sink: re_app::web::Sink,
        connect: Option<&str>,
        name: Option<&str>,
    ) -> Result<App, re_core::Diagnostic> {
        let files = self.project_files(re_app::web::browser_dirs(self.saved.clone(), sink));
        let config = AppConfig::load_from(&files)?;
        let mut app = App::open_files(files, config)?;
        match connect {
            Some(address) => {
                let mut options = NetOptions::new(NetRole::Client(address.to_owned()));
                options.name = name.map(str::to_owned);
                app.set_net(options)?;
            }
            None => app.persist_data(),
        }
        Ok(app)
    }

    /// Opens the project to play the replay at `replay` as a lockstep client
    /// of the server at `connect` (ADR-0050 §7), as `real run --headless
    /// --lockstep --replay R --connect A` does on desktop.
    ///
    /// # Errors
    ///
    /// As [`Session::run_replay`], and the diagnostic, rendered, when the game
    /// does not go online.
    pub fn net_run(
        &self,
        replay: &str,
        connect: &str,
        name: Option<&str>,
    ) -> Result<NetRun, String> {
        let (mut app, doc) = self.open(replay)?;
        let frames = doc.frames;
        app.play_replay(doc).map_err(|d| d.to_string())?;
        let mut options = NetOptions::new(NetRole::Client(connect.to_owned()));
        options.name = name.map(str::to_owned);
        options.lockstep = Some(1);
        app.set_net(options).map_err(|d| d.to_string())?;
        Ok(NetRun {
            dt: app.step(),
            app,
            frames,
            ran: 0,
        })
    }

    /// The project's name from `real.toml`, which names its saved data in the
    /// browser.
    ///
    /// # Errors
    ///
    /// The diagnostic, rendered, when `real.toml` is missing or invalid.
    pub fn project_name(&self) -> Result<String, String> {
        let files = self.project_files(WritableDirs::memory());
        let config = AppConfig::load_from(&files).map_err(|d| d.to_string())?;
        Ok(config.project.name)
    }

    /// The files as a [`ProjectFiles`], writing to `writable`.
    pub fn project_files(&self, writable: WritableDirs) -> ProjectFiles {
        let mut layers: Vec<Arc<dyn FileLayer>> = Vec::new();
        if let Some(a) = &self.archive {
            layers.push(a.clone());
        }
        layers.push(self.files.clone());
        ProjectFiles::new(layers, writable, None)
    }

    /// Opens the project and plays the replay at `replay` headless. Returns the
    /// run as JSON: `frames`, `state_hash`, and the error diagnostics.
    ///
    /// # Errors
    ///
    /// The diagnostic, rendered, when the project does not open or the replay
    /// cannot be read.
    pub fn run_replay(&self, replay: &str) -> Result<String, String> {
        let (mut app, doc) = self.open(replay)?;
        let report = app
            .run_headless(None, Some(doc))
            .map_err(|d| d.to_string())?;
        Ok(serde_json::json!({
            "frames": report.frames,
            "state_hash": report.state_hash,
            "errors": errors(&report.diagnostics),
        })
        .to_string())
    }

    /// [`Session::run_replay`] with a GPU device from `backends`: every
    /// `draw_every`-th frame (and the last) is drawn into an offscreen target the
    /// size of `real.toml [render]`. Adds `adapter` and `drawn` to the JSON, and
    /// the GPU's validation errors to `errors`, and `last_frame`: the number of
    /// distinct colours and the mean luma of the last frame drawn, read back from
    /// the GPU. The replay's checks are skipped (their count is
    /// `checks_skipped`): a screenshot check reads the GPU back by blocking,
    /// which a browser does not allow.
    ///
    /// # Errors
    ///
    /// As [`Session::run_replay`], and the device's diagnostic when no adapter
    /// matches `backends`.
    pub async fn run_replay_gpu(
        &self,
        replay: &str,
        backends: wgpu::Backends,
        draw_every: u64,
    ) -> Result<String, String> {
        let (mut app, mut doc) = self.open(replay)?;
        let skipped = doc.checks.len();
        doc.checks.clear();
        let gpu = Gpu::headless_async(backends)
            .await
            .map_err(|d| d.to_string())?;
        let adapter = gpu.adapter_line();
        let size = [
            app.config().render.width.max(1),
            app.config().render.height.max(1),
        ];
        let target = Capture::new(&gpu.device, size[0], size[1]).map_err(|d| d.to_string())?;
        let format = Capture::FORMAT;
        app.set_graphics(Graphics::new(gpu, format));
        let frames = doc.frames;
        app.play_replay(doc).map_err(|d| d.to_string())?;
        let mut diagnostics = Vec::new();
        let mut drawn = 0u64;
        for i in 0..frames {
            app.frame(app.step());
            if i % draw_every.max(1) == 0 || i + 1 == frames {
                if let Err(d) = app.draw_into(target.view(), size, 1.0) {
                    diagnostics.push(d);
                }
                drawn += 1;
            }
        }
        let mut last_frame = serde_json::Value::Null;
        if let Some(gfx) = app.graphics() {
            let read = target.read_async(&gfx.gpu().device, &gfx.gpu().queue);
            // The browser completes the map on its own; natively someone polls.
            #[cfg(not(target_arch = "wasm32"))]
            let _ = gfx.gpu().device.poll(wgpu::PollType::wait_indefinitely());
            match read.await {
                Ok(image) => {
                    let stats = image.stats([0, 0, 0, 255]);
                    last_frame = serde_json::json!({
                        "distinct_colors": stats.distinct_colors,
                        "luma_mean": stats.luma_mean,
                    });
                }
                Err(d) => diagnostics.push(d),
            }
            diagnostics.extend(gfx.take_errors());
        }
        diagnostics.extend(app.take_diagnostics());
        Ok(serde_json::json!({
            "frames": frames,
            "state_hash": app.state_hash().to_hex().to_string(),
            "errors": errors(&diagnostics),
            "adapter": adapter,
            "drawn": drawn,
            "checks_skipped": skipped,
            "last_frame": last_frame,
        })
        .to_string())
    }

    /// Opens the project and reads the replay at `replay`.
    fn open(&self, replay: &str) -> Result<(App, re_types::ReplayDoc), String> {
        let files = self.project_files(WritableDirs::memory());
        let doc = files
            .read_str(replay)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|e| format!("{replay}: {e}")))?;
        let config = AppConfig::load_from(&files).map_err(|d| d.to_string())?;
        let app = App::open_files(files, config).map_err(|d| d.to_string())?;
        Ok((app, doc))
    }
}

/// A replay played as a network client ([`Session::net_run`]), a step at a
/// time: a browser cannot wait inside a frame for its server's messages, which
/// arrive between turns of the page's event loop.
pub struct NetRun {
    app: App,
    dt: std::time::Duration,
    frames: u64,
    ran: u64,
}

impl std::fmt::Debug for NetRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetRun")
            .field("frames", &self.frames)
            .field("ran", &self.ran)
            .finish_non_exhaustive()
    }
}

impl NetRun {
    /// Runs frames until one waits for the server, or the run is over: all its
    /// frames ran, or the session ended. True once it is over.
    pub fn step(&mut self) -> bool {
        while !self.over() {
            if !self.app.try_frame(self.dt) {
                return false;
            }
            self.ran += 1;
        }
        true
    }

    fn over(&self) -> bool {
        self.ran >= self.frames || self.app.stopped()
    }

    /// Leaves the game and returns the run as JSON: `frames` run, the
    /// `state_hash` a desktop client's run report gives, and the error
    /// diagnostics.
    pub fn finish(&mut self) -> String {
        self.app.close_net();
        let diagnostics = self.app.take_diagnostics();
        serde_json::json!({
            "frames": self.ran,
            "state_hash": self.app.state_hash().to_hex().to_string(),
            "errors": errors(&diagnostics),
        })
        .to_string()
    }
}

/// The errors among `diagnostics`, rendered.
fn errors(diagnostics: &[re_core::Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(ToString::to_string)
        .collect()
}

#[cfg(target_arch = "wasm32")]
#[expect(
    unsafe_code,
    reason = "the module runs the C++ static constructors itself, once"
)]
mod wasm {
    use wasm_bindgen::prelude::*;

    unsafe extern "C" {
        fn __wasm_call_ctors();
    }

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console)]
        fn error(message: &str);
        #[wasm_bindgen(js_namespace = console)]
        fn trace(message: &str);
        /// Stores a saved file in IndexedDB (`None`: deletes it); boot.js defines it.
        #[wasm_bindgen(js_namespace = realEngine, js_name = persist)]
        fn js_persist(key: &str, bytes: Option<Vec<u8>>);
        /// Shows the run's state on the page; boot.js defines it.
        #[wasm_bindgen(js_namespace = realEngine, js_name = report)]
        fn js_report(kind: &str, message: &str);
    }

    fn persist(key: &str, bytes: Option<&[u8]>) {
        js_persist(key, bytes.map(<[u8]>::to_vec));
    }

    fn report(kind: &str, message: &str) {
        js_report(kind, message);
    }

    /// Runs the C++ static constructors once and sends panics to the console.
    /// The boot script calls it right after instantiating the module.
    #[wasm_bindgen]
    pub fn init() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: wasm-ld exports `__wasm_call_ctors` for a module that runs
            // its constructors itself; `ONCE` runs them once, before any C++ code.
            unsafe { __wasm_call_ctors() };
            std::panic::set_hook(Box::new(|info| {
                let message = format!("{} panic: {info}", re_app::codes::WEB_PANIC);
                trace(&message);
                report("error", &message);
            }));
        });
    }

    /// [`super::Session`] for JavaScript.
    #[wasm_bindgen]
    pub struct Session(super::Session);

    #[wasm_bindgen]
    impl Session {
        /// An empty project.
        #[wasm_bindgen(constructor)]
        pub fn new() -> Session {
            Session(super::Session::new())
        }

        /// [`super::Session::add_file`].
        #[wasm_bindgen(js_name = addFile)]
        pub fn add_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), JsError> {
            self.0.add_file(path, bytes).map_err(|e| JsError::new(&e))
        }

        /// [`super::Session::add_archive`].
        #[wasm_bindgen(js_name = addArchive)]
        pub fn add_archive(&mut self, bytes: Vec<u8>) -> Result<(), JsError> {
            self.0.add_archive(bytes).map_err(|e| JsError::new(&e))
        }

        /// [`super::Session::game_id`].
        #[wasm_bindgen(js_name = gameId)]
        pub fn game_id(&self) -> Result<String, JsError> {
            self.0.game_id().map_err(|e| JsError::new(&e))
        }

        /// [`super::Session::add_saved`].
        #[wasm_bindgen(js_name = addSaved)]
        pub fn add_saved(&mut self, key: &str, bytes: &[u8]) {
            self.0.add_saved(key, bytes);
        }

        /// Opens the project and plays it in a canvas appended to the page, joining
        /// the server at `connect` as `name` when given
        /// ([`super::Session::open_for_play`]). Returns once the page's event loop
        /// has taken over; later failures go to `realEngine.report("error", ...)`.
        pub fn play(&self, connect: Option<String>, name: Option<String>) -> Result<(), JsError> {
            let app = self
                .0
                .open_for_play(persist, connect.as_deref(), name.as_deref())
                .map_err(|d| JsError::new(&d.to_string()))?;
            re_app::web::play(app, report).map_err(|d| JsError::new(&d.to_string()))
        }

        /// [`super::Session::net_run`].
        #[wasm_bindgen(js_name = netRun)]
        pub fn net_run(
            &self,
            replay: &str,
            connect: &str,
            name: Option<String>,
        ) -> Result<NetRun, JsError> {
            self.0
                .net_run(replay, connect, name.as_deref())
                .map(NetRun)
                .map_err(|e| JsError::new(&e))
        }

        /// [`super::Session::project_name`].
        #[wasm_bindgen(js_name = projectName)]
        pub fn project_name(&self) -> Result<String, JsError> {
            self.0.project_name().map_err(|e| JsError::new(&e))
        }

        /// [`super::Session::run_replay`].
        #[wasm_bindgen(js_name = runReplay)]
        pub fn run_replay(&self, replay: &str) -> Result<String, JsError> {
            self.0.run_replay(replay).map_err(|e| JsError::new(&e))
        }

        /// [`super::Session::run_replay_gpu`] with WebGPU.
        #[wasm_bindgen(js_name = runReplayGpu)]
        pub async fn run_replay_gpu(
            &self,
            replay: String,
            draw_every: u32,
        ) -> Result<String, JsError> {
            self.0
                .run_replay_gpu(
                    &replay,
                    re_render::wgpu::Backends::BROWSER_WEBGPU,
                    u64::from(draw_every),
                )
                .await
                .map_err(|e| JsError::new(&e))
        }
    }

    impl Default for Session {
        fn default() -> Session {
            Session::new()
        }
    }

    /// [`super::NetRun`] for JavaScript: call `step()` until it returns true,
    /// yielding to the page's event loop in between, then `finish()`.
    #[wasm_bindgen]
    pub struct NetRun(super::NetRun);

    #[wasm_bindgen]
    impl NetRun {
        /// [`super::NetRun::step`].
        pub fn step(&mut self) -> bool {
            self.0.step()
        }

        /// [`super::NetRun::finish`].
        pub fn finish(&mut self) -> String {
            self.0.finish()
        }
    }
}
