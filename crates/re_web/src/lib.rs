//! `re_web`: Real Engine in the browser (ADR-0049).
//!
//! The boot script hands the project's files to [`Session::add_file`] and then
//! plays it with [`Session::open_for_play`] (`play` in JavaScript).
//! [`Session::run_replay`] is the headless entry point CI uses: it plays one
//! replay without a GPU, as `real verify` does, so the `state_hash` can be
//! compared with desktop's (equal once Jolt and glam are deterministic across
//! platforms, ADR-0036). [`Session::run_replay_gpu`] plays it with a GPU
//! device, drawing frames offscreen, and reports the GPU's errors too.

#![warn(missing_docs)]

use std::sync::Arc;

use re_app::{App, AppConfig, Graphics};
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath, WritableDirs};
use re_render::{Capture, Gpu, wgpu};

/// The project files received so far, and what to play.
#[derive(Debug)]
pub struct Session {
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

    /// Adds a file an earlier run persisted, keyed as
    /// [`re_app::web::Sink`] receives it (`data/<path>` or `crash/<path>`).
    pub fn add_saved(&mut self, key: &str, bytes: &[u8]) {
        self.saved.push((key.to_owned(), bytes.to_vec()));
    }

    /// Opens the project for play, with the saved files added so far and every
    /// later change of saved data and crash reports sent to `sink`.
    ///
    /// # Errors
    ///
    /// The diagnostic when the project does not open.
    pub fn open_for_play(&self, sink: re_app::web::Sink) -> Result<App, re_core::Diagnostic> {
        let files = self.project_files(re_app::web::browser_dirs(self.saved.clone(), sink));
        let config = AppConfig::load_from(&files)?;
        App::open_files(files, config)
    }

    /// The files as a [`ProjectFiles`], writing to `writable`.
    pub fn project_files(&self, writable: WritableDirs) -> ProjectFiles {
        ProjectFiles::new(vec![self.files.clone()], writable, None)
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
    /// the GPU. The replay's screenshot checks
    /// are skipped (their count is `screenshots_skipped`): a capture reads the
    /// GPU back by blocking, which a browser does not allow.
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
            "state_hash": app.world().state_hash().to_hex().to_string(),
            "errors": errors(&diagnostics),
            "adapter": adapter,
            "drawn": drawn,
            "screenshots_skipped": skipped,
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

/// The errors among `diagnostics`, rendered.
fn errors(diagnostics: &[re_core::Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(ToString::to_string)
        .collect()
}

#[cfg(target_arch = "wasm32")]
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

        /// [`super::Session::add_saved`].
        #[wasm_bindgen(js_name = addSaved)]
        pub fn add_saved(&mut self, key: &str, bytes: &[u8]) {
            self.0.add_saved(key, bytes);
        }

        /// Opens the project and plays it in a canvas appended to the page. Returns
        /// once the page's event loop has taken over; later failures go to
        /// `realEngine.report("error", ...)`.
        pub fn play(&self) -> Result<(), JsError> {
            let app = self
                .0
                .open_for_play(persist)
                .map_err(|d| JsError::new(&d.to_string()))?;
            re_app::web::play(app, report).map_err(|d| JsError::new(&d.to_string()))
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
}
