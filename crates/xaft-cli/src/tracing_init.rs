//! Tracing subscriber initialization.

use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use xaft_config::LogLevel;

/// Initialize the global tracing subscriber.
///
/// Sets up:
/// - A fmt layer for human-readable output to stderr
/// - An env-filter respecting `RUST_LOG` and the configured log level
/// - JSON output when `json_output` is `true`
///
/// Should be called exactly once at startup, after config is loaded.
pub fn init(log_level: &LogLevel, json_output: bool) {
    let filter = build_filter(log_level);

    // Use try_init so multiple calls (e.g. in tests) don't panic
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
    // Silently ignore "already initialized" — common in tests calling dispatch() multiple times
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
    let filter_str = format!(
        "{level},hyper=warn,reqwest=warn,h2=warn,rustls=warn,tokio=warn"
    );

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
