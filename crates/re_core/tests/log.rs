//! Logging tests, ported from the platform spike.

use std::io::Write;
use std::sync::{Arc, Mutex};

use re_core::log::{LogConfig, subscriber};

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

fn config(filter: &str, json: bool, span_timings: bool) -> LogConfig {
    LogConfig {
        filter: filter.into(),
        json,
        span_timings,
        // Keep the tests independent of the caller's RUST_LOG.
        env_override: false,
    }
}

/// tracing caches each callsite's interest process-wide, so tests that install
/// subscribers with different filters on parallel threads can see each other's
/// verdicts (a span or event dropped under the wrong filter). The tests take
/// this lock and rebuild the cache under their own subscriber.
static SERIAL: Mutex<()> = Mutex::new(());

fn run(config: LogConfig) -> String {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let buf = Buf::default();
    let sub = subscriber(&config, buf.clone()).expect("valid filter");
    tracing::subscriber::with_default(sub, || {
        tracing::callsite::rebuild_interest_cache();
        let span = tracing::info_span!("load_asset", path = "meshes/tree.glb", bytes = 1024u64);
        let _g = span.enter();
        tracing::debug!(target: "re_assets", "parsing");
        tracing::info!(meshes = 3, "imported");
        tracing::warn!(error = "missing texture", "fallback used");
    });
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

#[test]
fn text_output_with_filter_and_span_fields() {
    let out = run(config("info", false, true));
    print!("{out}");
    assert!(out.contains("load_asset{path=\"meshes/tree.glb\" bytes=1024}"));
    assert!(out.contains("imported meshes=3"));
    assert!(!out.contains("parsing"), "debug filtered out");
    assert!(out.contains("close time.busy="));
}

#[test]
fn per_target_directive_enables_debug() {
    let out = run(config("warn,re_assets=debug", false, false));
    print!("{out}");
    assert!(out.contains("parsing") && !out.contains("imported") && out.contains("fallback used"));
}

#[test]
fn json_lines_are_machine_readable() {
    let out = run(config("info", true, false));
    print!("{out}");
    for line in out.lines() {
        assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
    }
    assert!(
        out.contains(r#""meshes":3"#)
            && out.contains(r#""level":"WARN""#)
            && out.contains(r#""name":"load_asset""#)
    );
}

#[test]
fn invalid_filter_is_re0001() {
    let err = subscriber(&config("info,re_render=loud", false, false), Buf::default())
        .err()
        .expect("invalid directive");
    assert_eq!(err.code.to_string(), "RE0001");
    assert!(err.message.contains("re_render=loud"), "{}", err.message);
}
