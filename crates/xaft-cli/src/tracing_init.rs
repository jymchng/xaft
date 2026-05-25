//! Tracing subscriber initialization.

use std::fs::OpenOptions;
use std::path::Path;

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use xaft_config::LogLevel;

/// Initialize the global tracing subscriber writing to stderr.
pub fn init(log_level: &LogLevel, json_output: bool) {
    let filter = build_filter(log_level);
    let result = if json_output {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .try_init()
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_target(false)
                    .compact(),
            )
            .try_init()
    };
    if result.is_ok() {
        tracing::debug!(log_level = %log_level, json = json_output, "tracing initialized");
    }
}

/// Initialize tracing writing to `log_file` instead of stderr.
///
/// Used when the TUI is active so tracing output does not appear over the
/// ratatui alternate screen. Logs are still available for post-run inspection.
pub fn init_to_file(log_level: &LogLevel, log_file: &Path) {
    let filter = build_filter(log_level);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .unwrap_or_else(|_| {
            // If the file can't be opened, fall back to /dev/null
            OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .expect("/dev/null must exist")
        });

    let result = tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_target(false)
                .compact(),
        )
        .try_init();

    if result.is_ok() {
        tracing::debug!(log_level = %log_level, "tracing initialized to file");
    }
}

/// Build an `EnvFilter` from the configured log level.
///
/// `RUST_LOG` takes precedence if set, otherwise uses the config level.
fn build_filter(level: &LogLevel) -> EnvFilter {
    // RUST_LOG env var overrides everything
    if std::env::var("RUST_LOG").is_ok() {
        return EnvFilter::from_env("RUST_LOG");
    }

    // Otherwise use config log level, suppressing noisy crates
    let filter_str = format!("{level},hyper=warn,reqwest=warn,h2=warn,rustls=warn,tokio=warn");

    EnvFilter::try_new(&filter_str).unwrap_or_else(|_| EnvFilter::new("info"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_filter_info_level() {
        // Just verify it doesn't panic
        let filter = build_filter(&LogLevel::Info);
        drop(filter);
    }

    #[test]
    fn build_filter_debug_level() {
        let filter = build_filter(&LogLevel::Debug);
        drop(filter);
    }

    #[test]
    fn build_filter_trace_level() {
        let filter = build_filter(&LogLevel::Trace);
        drop(filter);
    }
}
