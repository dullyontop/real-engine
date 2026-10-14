//! Structured logging with `tracing`.
//!
//! The engine and games emit events and spans; one subscriber decides where they
//! go: human-readable text on a terminal, or one JSON object per line for tools and
//! AI agents. Binaries call [`init_logging`] exactly once at startup; tests build a
//! subscriber with [`subscriber`] and use `tracing::subscriber::with_default`.

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::codes;
use crate::diag::Diagnostic;

/// How logs are filtered and written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// `RUST_LOG`-style directives, for example `"info,re_render=debug"`.
    pub filter: String,
    /// One JSON object per line instead of human-readable text.
    pub json: bool,
    /// Also log span close events with their busy and idle time.
    pub span_timings: bool,
    /// When true, a valid `RUST_LOG` environment variable replaces `filter`.
    pub env_override: bool,
}

impl Default for LogConfig {
    /// `info`, human-readable, no span timings, `RUST_LOG` honoured.
    fn default() -> Self {
        LogConfig {
            filter: "info".into(),
            json: false,
            span_timings: false,
            env_override: true,
        }
    }
}

/// Builds the subscriber writing to `writer`.
///
/// It is returned rather than installed, so tests can use
/// `tracing::subscriber::with_default` and binaries install it once through
/// [`init_logging`]. Fails with `RE0001` when `config.filter` is not a valid
/// directive list.
pub fn subscriber<W>(
    config: &LogConfig,
    writer: W,
) -> Result<Box<dyn tracing::Subscriber + Send + Sync>, Diagnostic>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = build_filter(config)?;
    let spans = if config.span_timings {
        FmtSpan::CLOSE
    } else {
        FmtSpan::NONE
    };
    let registry = tracing_subscriber::registry().with(filter);
    Ok(if config.json {
        Box::new(
            registry.with(
                fmt::layer()
                    .json()
                    .with_span_events(spans)
                    .with_writer(writer),
            ),
        )
    } else {
        Box::new(
            registry.with(
                fmt::layer()
                    .with_ansi(false)
                    .with_span_events(spans)
                    .with_writer(writer),
            ),
        )
    })
}

/// Installs the process-wide subscriber, writing to standard error.
///
/// Fails with `RE0001` for an invalid filter, or `RE0002` when a global subscriber
/// is already installed.
pub fn init_logging(config: LogConfig) -> Result<(), Diagnostic> {
    let subscriber = subscriber(&config, std::io::stderr)?;
    tracing::subscriber::set_global_default(subscriber).map_err(|_| {
        Diagnostic::error(
            codes::LOG_ALREADY_INITIALISED,
            "logging is already initialised in this process",
        )
        .with_hint("call init_logging once, at startup")
    })
}

fn build_filter(config: &LogConfig) -> Result<EnvFilter, Diagnostic> {
    if config.env_override
        && let Ok(filter) = EnvFilter::try_from_default_env()
    {
        return Ok(filter);
    }
    EnvFilter::try_new(&config.filter).map_err(|e| {
        Diagnostic::error(
            codes::LOG_FILTER_INVALID,
            format!("invalid log filter `{}`: {e}", config.filter),
        )
        .with_hint("write comma-separated directives such as `info,re_render=debug`")
    })
}
